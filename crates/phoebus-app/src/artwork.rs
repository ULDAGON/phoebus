//! Album cover cache: `AlbumKey` → GPU texture, decoded off the frame path.
//!
//! Rules that keep scrolling at 60 fps:
//! * a PNG is decoded on a **background thread**, never while a frame is being laid out;
//! * a texture is uploaded **once** and then reused at every size (48 px LCD, 180 px card,
//!   232 px detail header);
//! * a cover that is missing or unreadable is remembered as such, so the loader is asked
//!   exactly once per album per scan;
//! * at most [`MAX_TEXTURES`] textures are alive at once. A cover is up to 600 × 600 RGBA
//!   (~1.4 MB of VRAM), so an unbounded cache would pin gigabytes on a large library.
//!   The least recently *painted* textures are dropped past the cap and simply reload if
//!   they come back on screen.
//!
//! Until a texture arrives the caller gets a placeholder: a `BG2` square with a `TEXT_LOW`
//! `♪`. That is also the permanent look of an album with no artwork.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crossbeam_channel::{Receiver, Sender};
use egui::{Align2, Context, Rect, Response, Sense, TextureHandle, TextureId, Ui, Vec2};
use phoebus_core::AlbumKey;

use crate::theme;

/// How many decoded covers stay resident. One 600 × 600 RGBA texture is ~1.4 MB, so this
/// caps the cache at roughly 70 MB — several times what any single frame can show (a
/// 1280 px Albums grid draws ~30 cards, the drawer and the LCD a handful more), which is
/// what keeps eviction from ever touching something that is on screen.
const MAX_TEXTURES: usize = 48;

/// What the loader thread is asked to do.
struct Job {
    key: AlbumKey,
    path: PathBuf,
    /// The [`Artwork::generation`] this job was queued under.
    generation: u64,
}

/// What comes back. `image` is `None` when the file is missing or undecodable.
struct Done {
    key: AlbumKey,
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

/// Cache state for one album.
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
    slots: HashMap<AlbumKey, Slot>,
    jobs: Sender<Job>,
    done: Receiver<Done>,
    /// Generation counter, bumped by [`Artwork::reset`]; stale replies are dropped.
    generation: u64,
    /// Keys with a job in flight *for the current generation*. Drives [`Artwork::is_idle`].
    pending: HashSet<AlbumKey>,
    /// Frames pumped so far — the clock the LRU ages textures against.
    frame: u64,
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
        }
    }

    /// Point the cache at a (new) cover directory and forget every texture. Called when a
    /// scan finishes, because covers may have been regenerated.
    pub fn reset(&mut self, covers_dir: PathBuf) {
        self.covers_dir = covers_dir;
        self.slots.clear();
        self.pending.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    /// Drain the loader thread and upload whatever arrived. Call once per frame from
    /// `App::logic` — this is the only place a texture is created.
    pub fn pump(&mut self, ctx: &Context) {
        self.frame = self.frame.wrapping_add(1);
        let mut uploaded = false;
        while let Ok(done) = self.done.try_recv() {
            if !self.accept(&done.key, done.generation) {
                continue; // decoded before the last rescan
            }
            let slot = match done.image {
                Some(image) => {
                    let name = format!("cover-{:016x}", done.key.hash64());
                    Slot::Ready(Ready {
                        tex: ctx.load_texture(name, image, egui::TextureOptions::LINEAR),
                        used: self.frame,
                    })
                }
                None => Slot::Missing,
            };
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
    fn accept(&mut self, key: &AlbumKey, generation: u64) -> bool {
        if generation != self.generation {
            return false;
        }
        self.pending.remove(key);
        true
    }

    /// Drop the least recently painted textures past [`MAX_TEXTURES`].
    ///
    /// Only `Ready` slots cost VRAM; `Missing` slots are one enum variant each and are
    /// what stops the loader being asked again for an album with no art. A dropped
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
        let mut ages: Vec<(u64, AlbumKey)> = self
            .slots
            .iter()
            .filter_map(|(key, slot)| match slot {
                // Never evict something uploaded or painted on this very frame.
                Slot::Ready(ready) if ready.used != self.frame => Some((ready.used, key.clone())),
                _ => None,
            })
            .collect();
        ages.sort_unstable_by_key(|(used, _)| *used);
        for (_, key) in ages.into_iter().take(live - MAX_TEXTURES) {
            self.slots.remove(&key);
        }
    }

    /// The texture for an album, requesting a load the first time it is asked for.
    ///
    /// Returns `None` while the cover is loading, and forever if there is none.
    pub fn texture(&mut self, key: &AlbumKey) -> Option<TextureId> {
        let frame = self.frame;
        if let Some(slot) = self.slots.get_mut(key) {
            return match slot {
                Slot::Ready(ready) => {
                    ready.used = frame;
                    Some(ready.tex.id())
                }
                _ => None,
            };
        }
        if self.covers_dir.as_os_str().is_empty() {
            return None;
        }
        let path = self.covers_dir.join(key.cover_file_name());
        self.slots.insert(key.clone(), Slot::Loading);
        self.pending.insert(key.clone());
        if self
            .jobs
            .send(Job {
                key: key.clone(),
                path,
                generation: self.generation,
            })
            .is_err()
        {
            self.slots.insert(key.clone(), Slot::Missing);
            self.pending.remove(key);
        }
        None
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

fn decode(path: &std::path::Path) -> Option<egui::ColorImage> {
    if !path.exists() {
        return None;
    }
    let img = match image::open(path) {
        Ok(img) => img,
        Err(e) => {
            log::warn!("artwork: {} failed to decode: {e}", path.display());
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

/// Draw an album cover of `size` at the current cursor and return its response.
///
/// `key` of `None` (or a cover that has not loaded) paints the placeholder.
pub fn cover(ui: &mut Ui, art: &mut Artwork, key: Option<&AlbumKey>, size: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    paint_cover(ui, art, key, rect);
    response
}

/// Paint an album cover into an already-allocated rect.
pub fn paint_cover(ui: &Ui, art: &mut Artwork, key: Option<&AlbumKey>, rect: Rect) {
    let tex = key.and_then(|k| art.texture(k));
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
        let key = AlbumKey::new("HOME", "Odyssey");

        assert!(art.texture(&key).is_none(), "the first ask only enqueues");
        let stale = art.generation;
        assert!(!art.is_idle());

        art.reset(PathBuf::from("/nonexistent/covers")); // a rescan finished
        assert!(art.is_idle(), "the old generation stops counting");
        assert!(art.texture(&key).is_none(), "the grid asks again");
        assert!(!art.is_idle());

        assert!(
            !art.accept(&key, stale),
            "the pre-rescan decode must not be installed"
        );
        assert!(!art.is_idle(), "…and must not clear the fresh request");
        assert!(
            art.accept(&key, art.generation),
            "the fresh decode must be installed"
        );
        assert!(art.is_idle());
    }

    #[test]
    fn a_key_with_no_covers_dir_is_never_enqueued() {
        let mut art = Artwork::new();
        assert!(art.texture(&AlbumKey::new("HOME", "Odyssey")).is_none());
        assert!(art.is_idle(), "nothing to wait for before the first scan");
    }
}
