//! Cover cache: [`CoverId`] → GPU texture, decoded off the frame path.
//!
//! Keyed by the picture, not the album: a song paints its own cover, an album whose songs
//! all carry one picture shares one texture between every place that shows it, and a
//! compilation whose songs each carry their own gets one per song ([`paint_mosaic`] for
//! its card). The scan names each cached PNG by the same id, so a texture can never be
//! out of date for its key — which is what lets [`Artwork::refresh`] keep every uploaded
//! one across a rescan.
//!
//! Rules that keep scrolling at 60 fps:
//! * a PNG is decoded on a **background thread**, never while a frame is being laid out;
//! * a texture is uploaded **once** and then reused at every size (48 px LCD, 180 px card,
//!   232 px detail header);
//! * a cover that is missing or unreadable is remembered as such, so the loader is asked
//!   exactly once per cover per scan;
//! * at most [`MAX_TEXTURES`] textures stay alive once they are off screen. A cover is up
//!   to 600 × 600 RGBA (~1.4 MB of VRAM), so an unbounded cache would pin gigabytes on a
//!   large library. The least recently *painted* textures are dropped past the cap and
//!   simply reload if they come back on screen; what is on screen is never dropped.
//!
//! Until a texture arrives the caller gets a placeholder: a `BG2` square with a `TEXT_LOW`
//! `♪`. That is also the permanent look of a song or album with no artwork.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crossbeam_channel::{Receiver, Sender};
use egui::{Align2, Context, Rect, Response, Sense, TextureHandle, TextureId, Ui, Vec2};
use phoebus_core::{CoverId, MOSAIC_TILES};

use crate::theme;

/// How many decoded covers stay resident once they are off screen. One 600 × 600 RGBA
/// texture is ~1.4 MB, so this caps the off-screen part of the cache at roughly 90 MB.
///
/// A soft cap: a texture painted on this frame or the last one is never evicted
/// ([`Artwork::evict`]), however many there are. Per-song covers and album mosaics made a
/// single frame able to show more than any fixed cap — a search for a compilation paints
/// forty-odd song covers and mosaic tiles, the queue drawer and the LCD more on top, and
/// an Albums grid of mosaics four per card — and evicting what is on screen meant
/// decoding and repainting it on every frame. What is on screen is bounded by the screen.
const MAX_TEXTURES: usize = 64;

/// What the loader thread is asked to do.
struct Job {
    key: CoverId,
    path: PathBuf,
    /// The [`Artwork::generation`] this job was queued under.
    generation: u64,
}

/// What comes back. `image` is `None` when the file is missing or undecodable.
struct Done {
    key: CoverId,
    image: Option<egui::ColorImage>,
    /// Copied back from [`Job::generation`] — a reply decoded before the last rescan
    /// carries the old value and is dropped on arrival.
    generation: u64,
}

/// An uploaded cover plus the frame it was last painted on.
struct Ready {
    tex: TextureHandle,
    /// [`Artwork::frame`] at the last [`Artwork::texture`] call — the LRU key.
    used: u64,
}

/// Cache state for one cover.
enum Slot {
    /// A job is in flight.
    Loading,
    /// Uploaded and ready.
    Ready(Ready),
    /// No cover on disk, or it could not be decoded — draw the placeholder forever.
    Missing,
}

/// The cover cache. One instance lives in the app; pass it around as `&mut`.
pub struct Artwork {
    covers_dir: PathBuf,
    slots: HashMap<CoverId, Slot>,
    jobs: Sender<Job>,
    done: Receiver<Done>,
    /// Generation counter, bumped by [`Artwork::reset`]; stale replies are dropped.
    generation: u64,
    /// Keys with a job in flight *for the current generation*. Drives [`Artwork::is_idle`].
    pending: HashSet<CoverId>,
    /// Frames pumped so far — the clock the LRU ages textures against.
    frame: u64,
    /// New cover → the cover it replaces, for a song or album tile whose art a rescan
    /// changed ([`Artwork::refresh`]). While the new one loads, the old texture is painted
    /// in its place, so a re-tag never shows the placeholder in between.
    standins: HashMap<CoverId, CoverId>,
}

impl Artwork {
    /// Start the loader thread. The thread lives as long as the app.
    pub fn new() -> Artwork {
        let (jobs_tx, jobs_rx) = crossbeam_channel::unbounded::<Job>();
        let (done_tx, done_rx) = crossbeam_channel::unbounded::<Done>();
        if let Err(e) = std::thread::Builder::new()
            .name("phoebus-artwork".to_string())
            .spawn(move || loader(&jobs_rx, &done_tx))
        {
            log::warn!("artwork: could not spawn the loader thread, covers stay blank: {e}");
        }
        Artwork {
            covers_dir: PathBuf::new(),
            slots: HashMap::new(),
            jobs: jobs_tx,
            done: done_rx,
            generation: 0,
            pending: HashSet::new(),
            frame: 0,
            standins: HashMap::new(),
        }
    }

    /// Point the cache at a (new) cover directory and forget every texture. Called when a
    /// library replaces the one on screen wholesale — at launch, and when the root changes.
    pub fn reset(&mut self, covers_dir: PathBuf) {
        self.covers_dir = covers_dir;
        self.slots.clear();
        self.standins.clear();
        self.pending.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    /// A background rescan finished: forget what it may have changed, and nothing else.
    ///
    /// Every uploaded cover stays. A cache file is named by its picture, so the scan never
    /// rewrites one with other art — new art is a new id, which simply is not loaded yet.
    /// One the library no longer uses just ages out of the LRU.
    ///
    /// `replaced` pairs each cover the rescan replaced with its successor, `(old, new)`
    /// ([`phoebus_core::Library::replaced_covers`]): a re-tagged song, a changed tile of an
    /// album's mosaic. Until the new one has loaded, the old texture is painted in its
    /// place, so the swap shows no placeholder anywhere — the first frame after it paints
    /// exactly what the frame before did, and each new cover replaces its old one as it
    /// arrives. A new cover that turns out to be missing ends as the placeholder only then.
    ///
    /// Two kinds of slot go. `Missing` ones, because "no cover" may only have meant "not
    /// written yet" — a first scan still running, or a cover cache that was cleared and
    /// that the scan just refilled; each is asked for once more, the next time it is
    /// painted. And `Loading` ones, because the generation bump drops every reply in
    /// flight, and a slot left at `Loading` would then never be asked for again.
    pub fn refresh(&mut self, replaced: impl IntoIterator<Item = (CoverId, CoverId)>) {
        self.slots.retain(|_, slot| matches!(slot, Slot::Ready(_)));
        self.standins.clear();
        for (old, new) in replaced {
            if matches!(self.slots.get(&old), Some(Slot::Ready(_)))
                && !self.slots.contains_key(&new)
            {
                self.standins.insert(new, old);
            }
        }
        self.pending.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    /// Drain the loader thread and upload whatever arrived. Call once per frame from
    /// `App::logic` — this is the only place a texture is created.
    pub fn pump(&mut self, ctx: &Context) {
        self.frame = self.frame.wrapping_add(1);
        let mut uploaded = false;
        while let Ok(done) = self.done.try_recv() {
            if !self.accept(done.key, done.generation) {
                continue; // decoded before the last rescan
            }
            let slot = match done.image {
                Some(image) => {
                    let name = format!("cover-{}", done.key);
                    Slot::Ready(Ready {
                        tex: ctx.load_texture(name, image, egui::TextureOptions::LINEAR),
                        used: self.frame,
                    })
                }
                None => Slot::Missing,
            };
            self.standins.remove(&done.key);
            self.slots.insert(done.key, slot);
            uploaded = true;
        }
        self.evict();
        // Keep the UI ticking only while covers are in flight: a newly uploaded texture
        // has to be painted, and a pending one has to be collected.
        if uploaded {
            ctx.request_repaint();
        } else if !self.pending.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    /// Should this reply be installed?
    ///
    /// The check is on the generation the *job* was queued under, not on "is this key
    /// pending now": after a rescan the visible grid re-requests the same keys
    /// immediately, so a key being pending says nothing about which scan's cover file the
    /// reply in hand was decoded from.
    fn accept(&mut self, key: CoverId, generation: u64) -> bool {
        if generation != self.generation {
            return false;
        }
        self.pending.remove(&key);
        true
    }

    /// Drop the least recently painted textures past [`MAX_TEXTURES`] — but never one
    /// painted on this frame or the last.
    ///
    /// The last frame counts because this runs from [`Artwork::pump`], at the start of a
    /// frame, after the clock has moved on and before anything is painted: what the
    /// previous frame painted is what is on screen.
    ///
    /// Only `Ready` slots cost VRAM; `Missing` slots are one enum variant each and are
    /// what stops the loader being asked again for a cover that is not there. A dropped
    /// `TextureHandle` frees the GPU texture, and the next [`Artwork::texture`] call for
    /// that key simply re-requests the decode.
    fn evict(&mut self) {
        let live = self
            .slots
            .values()
            .filter(|s| matches!(s, Slot::Ready(_)))
            .count();
        if live <= MAX_TEXTURES {
            return;
        }
        let mut ages: Vec<(u64, CoverId)> = self
            .slots
            .iter()
            .filter_map(|(key, slot)| match slot {
                Slot::Ready(ready) if self.frame.wrapping_sub(ready.used) > 1 => {
                    Some((ready.used, *key))
                }
                _ => None,
            })
            .collect();
        ages.sort_unstable_by_key(|(used, _)| *used);
        for (_, key) in ages.into_iter().take(live - MAX_TEXTURES) {
            self.slots.remove(&key);
        }
    }

    /// The texture of a cover, requesting a load the first time it is asked for.
    ///
    /// Returns `None` while the cover is loading, and forever if there is none — except
    /// that a cover standing in for it after a rescan ([`Artwork::refresh`]) is returned
    /// while it loads.
    pub fn texture(&mut self, key: CoverId) -> Option<TextureId> {
        let frame = self.frame;
        if let Some(slot) = self.slots.get_mut(&key) {
            return match slot {
                Slot::Ready(ready) => {
                    ready.used = frame;
                    Some(ready.tex.id())
                }
                Slot::Loading => self.standin(key),
                Slot::Missing => None,
            };
        }
        if self.covers_dir.as_os_str().is_empty() {
            return None;
        }
        let path = self.covers_dir.join(key.file_name());
        self.slots.insert(key, Slot::Loading);
        self.pending.insert(key);
        if self
            .jobs
            .send(Job {
                key,
                path,
                generation: self.generation,
            })
            .is_err()
        {
            self.slots.insert(key, Slot::Missing);
            self.pending.remove(&key);
            return None;
        }
        self.standin(key)
    }

    /// The texture of the cover `key` replaced, while `key` loads.
    fn standin(&mut self, key: CoverId) -> Option<TextureId> {
        let old = *self.standins.get(&key)?;
        match self.slots.get_mut(&old) {
            Some(Slot::Ready(ready)) => {
                ready.used = self.frame;
                Some(ready.tex.id())
            }
            _ => None,
        }
    }

    /// True when no cover decode is in flight. `--shot-once` waits for this so screenshots
    /// never catch a half-loaded grid.
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty()
    }
}

impl Default for Artwork {
    fn default() -> Self {
        Artwork::new()
    }
}

fn loader(jobs: &Receiver<Job>, done: &Sender<Done>) {
    while let Ok(job) = jobs.recv() {
        let image = decode(&job.path);
        if image.is_none() {
            log::debug!("artwork: no usable cover at {}", job.path.display());
        }
        if done
            .send(Done {
                key: job.key,
                image,
                generation: job.generation,
            })
            .is_err()
        {
            return; // the app is gone
        }
    }
}

/// Decode a cached cover. A file that is there but does not decode is deleted: the cache
/// trusts every file under a cover's name, so a damaged one would otherwise be painted as
/// `♪` forever. Gone, it is written again by the next scan. Only ever called with a path
/// inside the covers dir that a [`CoverId`] names.
fn decode(path: &std::path::Path) -> Option<egui::ColorImage> {
    if !path.exists() {
        return None;
    }
    let img = match image::open(path) {
        Ok(img) => img,
        Err(e) => {
            log::warn!(
                "artwork: {} failed to decode ({e}); removed, the next scan writes it again",
                path.display()
            );
            if let Err(e) = std::fs::remove_file(path) {
                log::warn!("artwork: {} stayed behind: {e}", path.display());
            }
            return None;
        }
    };
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    if size[0] == 0 || size[1] == 0 {
        return None;
    }
    Some(egui::ColorImage::from_rgba_unmultiplied(
        size,
        rgba.as_raw(),
    ))
}

/// Paint a cover into an already-allocated rect.
///
/// `None` (or a cover that has not loaded) paints the placeholder.
pub fn paint_cover(ui: &Ui, art: &mut Artwork, cover: Option<CoverId>, rect: Rect) {
    let tex = cover.and_then(|c| art.texture(c));
    let painter = ui.painter_at(rect);
    match tex {
        Some(id) => {
            painter.image(
                id,
                rect,
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        None => {
            painter.rect_filled(rect, theme::corner(), theme::p().bg2);
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                theme::GLYPH_NOTE,
                theme::font_icon((rect.width() * 0.32).clamp(11.0, 48.0)),
                theme::p().text_low,
            );
        }
    }
}

/// How a set of distinct covers fills one square.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    /// No cover at all: the `♪` placeholder.
    Placeholder,
    /// One cover, full size — an ordinary album, or a playlist of one album.
    Single(CoverId),
    /// A 2 × 2 mosaic, tiles in reading order. Fewer than four covers repeat from the
    /// first: two fill a column each (`a b / a b`), three put the first one back in the
    /// last corner (`a b / c a`).
    Grid([CoverId; MOSAIC_TILES]),
}

impl Layout {
    fn of(covers: &[CoverId]) -> Layout {
        match covers {
            [] => Layout::Placeholder,
            [one] => Layout::Single(*one),
            many => Layout::Grid(std::array::from_fn(|tile| many[tile % many.len()])),
        }
    }
}

/// Paint a set of distinct covers into a square: one cover, a 2 × 2 mosaic of the first
/// [`MOSAIC_TILES`], or the `♪` placeholder ([`Layout`]). The playlist header, and the card
/// and header of an album whose songs carry more than one picture.
pub fn paint_mosaic(ui: &Ui, art: &mut Artwork, covers: &[CoverId], rect: Rect) {
    let tiles = match Layout::of(covers) {
        Layout::Placeholder => return paint_cover(ui, art, None, rect),
        Layout::Single(cover) => return paint_cover(ui, art, Some(cover), rect),
        Layout::Grid(tiles) => tiles,
    };
    let half = rect.width() * 0.5;
    for (tile, cover) in tiles.into_iter().enumerate() {
        let origin = egui::pos2(
            rect.left() + (tile % 2) as f32 * half,
            rect.top() + (tile / 2) as f32 * half,
        );
        paint_cover(
            ui,
            art,
            Some(cover),
            Rect::from_min_size(origin, Vec2::splat(half)),
        );
    }
    let painter = ui.painter_at(rect);
    painter.line_segment(
        [
            egui::pos2(rect.center().x, rect.top()),
            egui::pos2(rect.center().x, rect.bottom()),
        ],
        theme::hairline(),
    );
    painter.line_segment(
        [
            egui::pos2(rect.left(), rect.center().y),
            egui::pos2(rect.right(), rect.center().y),
        ],
        theme::hairline(),
    );
}

/// [`paint_mosaic`] at the cursor, `size` square — the album header.
pub fn mosaic(ui: &mut Ui, art: &mut Artwork, covers: &[CoverId], size: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    paint_mosaic(ui, art, covers, rect);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression this guards: after a rescan the visible grid re-requests the same
    /// keys on the very next frame, so "is this key pending" cannot tell a pre-rescan
    /// reply from a fresh one. Only the generation the job was queued under can.
    #[test]
    fn a_reply_decoded_before_a_rescan_is_dropped_and_the_fresh_one_kept() {
        let mut art = Artwork::new();
        art.reset(PathBuf::from("/nonexistent/covers"));
        let key = CoverId(0x0d55_e700);

        assert!(art.texture(key).is_none(), "the first ask only enqueues");
        let stale = art.generation;
        assert!(!art.is_idle());

        art.reset(PathBuf::from("/nonexistent/covers")); // a rescan finished
        assert!(art.is_idle(), "the old generation stops counting");
        assert!(art.texture(key).is_none(), "the grid asks again");
        assert!(!art.is_idle());

        assert!(
            !art.accept(key, stale),
            "the pre-rescan decode must not be installed"
        );
        assert!(!art.is_idle(), "…and must not clear the fresh request");
        assert!(
            art.accept(key, art.generation),
            "the fresh decode must be installed"
        );
        assert!(art.is_idle());
    }

    /// A quiet rescan keeps every uploaded cover — a cover's file never changes under its
    /// name — and gives every placeholder one more chance.
    #[test]
    fn a_refresh_keeps_every_uploaded_cover_and_retries_the_unsettled_ones() {
        let ctx = Context::default();
        let mut art = Artwork::new();
        art.reset(PathBuf::from("/nonexistent/covers"));
        let [kept, other, missing, loading] = [1, 2, 3, 4].map(CoverId);
        let image = || egui::ColorImage::filled([1, 1], egui::Color32::WHITE);
        for key in [kept, other] {
            let tex = ctx.load_texture("t", image(), egui::TextureOptions::LINEAR);
            art.slots.insert(key, Slot::Ready(Ready { tex, used: 0 }));
        }
        art.slots.insert(missing, Slot::Missing);
        assert!(art.texture(loading).is_none(), "enqueued");
        let before = art.generation;

        art.refresh([]);
        for key in [kept, other] {
            assert!(art.texture(key).is_some(), "{key} stays on screen");
        }
        assert!(art.generation != before, "in-flight replies are dropped");
        for key in [missing, loading] {
            assert!(!art.slots.contains_key(&key), "{key} is asked for again");
        }
    }

    fn ready(ctx: &Context) -> (Slot, TextureId) {
        let image = egui::ColorImage::filled([1, 1], egui::Color32::WHITE);
        let tex = ctx.load_texture("t", image, egui::TextureOptions::LINEAR);
        let id = tex.id();
        (Slot::Ready(Ready { tex, used: 0 }), id)
    }

    /// A re-tag gives a song a new cover id. The quiet swap must not blank it: the old
    /// texture is painted until the new one has landed, and a new cover that is missing
    /// ends as the placeholder only then.
    #[test]
    fn a_replaced_cover_is_painted_until_its_successor_lands() {
        let ctx = Context::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let [old, new, old_gone, new_gone, untouched] = [1, 2, 3, 4, 5].map(CoverId);
        image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]))
            .save(dir.path().join(new.file_name()))
            .expect("write cover");

        let mut art = Artwork::new();
        art.reset(dir.path().to_path_buf());
        let (slot, old_tex) = ready(&ctx);
        art.slots.insert(old, slot);
        let (slot, old_gone_tex) = ready(&ctx);
        art.slots.insert(old_gone, slot);

        art.refresh([(old, new), (old_gone, new_gone), (untouched, CoverId(6))]);
        assert_eq!(art.texture(new), Some(old_tex), "the old art while loading");
        assert_eq!(art.texture(new_gone), Some(old_gone_tex));
        assert_eq!(
            art.texture(CoverId(6)),
            None,
            "nothing to stand in with: the placeholder, as for any first load"
        );

        for _ in 0..500 {
            art.pump(&ctx);
            if art.is_idle() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(art.is_idle(), "the loads landed");
        let landed = art.texture(new).expect("the new cover");
        assert!(landed != old_tex, "the new texture replaced the old one");
        assert_eq!(
            art.texture(new_gone),
            None,
            "a missing new cover is missing"
        );
        assert_eq!(
            art.texture(old),
            Some(old_tex),
            "the old cover is still its own"
        );
    }

    /// A damaged cache file is not painted as `♪` forever: the loader deletes it, so the
    /// next scan finds the cover missing and writes it again.
    #[test]
    fn a_cache_file_that_does_not_decode_is_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bad = dir.path().join(CoverId(1).file_name());
        std::fs::write(&bad, b"").expect("write");
        assert!(decode(&bad).is_none());
        assert!(!bad.exists(), "an empty file goes");
        std::fs::write(&bad, b"\x89PNG\r\n\x1a\ntruncated").expect("write");
        assert!(decode(&bad).is_none());
        assert!(!bad.exists(), "a damaged file goes");

        let good = dir.path().join(CoverId(2).file_name());
        image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 255, 255]))
            .save(&good)
            .expect("write cover");
        assert!(decode(&good).is_some());
        assert!(good.exists(), "a sound file stays");
        assert!(decode(&dir.path().join(CoverId(3).file_name())).is_none());
    }

    /// More covers on screen than the cap: none of them is evicted (they would be decoded
    /// and repainted every frame). The cap only ever drops covers that are off screen.
    #[test]
    fn eviction_never_drops_a_cover_that_is_on_screen() {
        let ctx = Context::default();
        let mut art = Artwork::new();
        art.reset(PathBuf::from("/nonexistent/covers"));
        art.frame = 100;
        let on_screen = MAX_TEXTURES + 20;
        let off_screen = 30;
        for i in 0..on_screen + off_screen {
            let (slot, _) = ready(&ctx);
            art.slots.insert(CoverId(i as u64), slot);
            if let Some(Slot::Ready(r)) = art.slots.get_mut(&CoverId(i as u64)) {
                // Painted on the last frame, or long ago.
                r.used = if i < on_screen {
                    99
                } else {
                    (i - on_screen) as u64
                };
            }
        }
        art.evict();
        for i in 0..on_screen {
            assert!(
                matches!(art.slots.get(&CoverId(i as u64)), Some(Slot::Ready(_))),
                "on-screen cover {i} was evicted"
            );
        }
        assert!(
            (on_screen..on_screen + off_screen)
                .all(|i| !art.slots.contains_key(&CoverId(i as u64))),
            "the off-screen ones go while the screen alone is over the cap"
        );

        // Off screen again: the cap applies, least recently painted first.
        art.frame = 200;
        art.evict();
        let live = art
            .slots
            .values()
            .filter(|s| matches!(s, Slot::Ready(_)))
            .count();
        assert_eq!(live, MAX_TEXTURES);
    }

    #[test]
    fn a_key_with_no_covers_dir_is_never_enqueued() {
        let mut art = Artwork::new();
        assert!(art.texture(CoverId(7)).is_none());
        assert!(art.is_idle(), "nothing to wait for before the first scan");
    }

    /// One distinct cover is one cover, whatever else the album holds; more is a mosaic of
    /// the first four, and two or three repeat to fill it the way playlists always have.
    #[test]
    fn one_cover_is_painted_whole_and_more_make_a_mosaic() {
        let [a, b, c, d, e] = [1, 2, 3, 4, 5].map(CoverId);
        assert_eq!(Layout::of(&[]), Layout::Placeholder);
        assert_eq!(Layout::of(&[a]), Layout::Single(a));
        assert_eq!(Layout::of(&[a, b]), Layout::Grid([a, b, a, b]));
        assert_eq!(Layout::of(&[a, b, c]), Layout::Grid([a, b, c, a]));
        assert_eq!(Layout::of(&[a, b, c, d]), Layout::Grid([a, b, c, d]));
        assert_eq!(
            Layout::of(&[a, b, c, d, e]),
            Layout::Grid([a, b, c, d]),
            "only the first four"
        );
    }
}
