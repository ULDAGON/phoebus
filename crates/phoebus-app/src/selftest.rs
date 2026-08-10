//! `phoebus --selftest` — Gate G2's acceptance test, run headless.
//!
//! It touches no egui and opens no window. It scans the library, checks the tags, durations
//! and cached covers it found, drives the real audio engine at volume 0 through a decode +
//! seek probe, and round-trips playlists, favorites and app state **against temporary
//! directories** so a self-test can never disturb the user's `playlists.json` or
//! `favorites.json`.
//!
//! Every check prints one `PASS <name> <detail>` or `FAIL <name> <detail>` line; the exit
//! code is 1 if anything failed.
//!
//! **Isolation invariant.** The only directory the self-test writes to that it did not
//! create itself is the *active* app-data directory's cover cache, refreshed by the scan.
//! Both directories come from the environment the same way the app resolves them, so
//! `PHOEBUS_DATA=<tmp> PHOEBUS_LIBRARY=<tmp> phoebus --selftest` leaves `~/.phoebus`
//! completely untouched — nothing here ever falls back to the real home.
//!
//! The scan checks only demand that the library is non-empty, because they run against
//! whatever library the machine has. Set [`ENV_EXPECT`] to gate on an exact shape.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use phoebus_audio::{Event, EventKind, PlayerHandle};
use phoebus_core::{AppState, Dirs, Favorites, Library, PlaylistStore, Repeat, TrackId, scanner};

/// Comma-separated `albums,artists,tracks` minimums for the scan checks.
///
/// The defaults only assert that the scan found *something*, because the self-test runs
/// against whatever library the machine actually has. A release gate that wants the
/// seeded library's exact shape sets this: `PHOEBUS_SELFTEST_EXPECT="3,3,38"`.
pub const ENV_EXPECT: &str = "PHOEBUS_SELFTEST_EXPECT";

/// Fewest albums, artists and tracks a library must have for the scan checks to pass when
/// [`ENV_EXPECT`] is not set: enough to catch an empty or broken scan, no more.
const DEFAULT_EXPECT: Expect = Expect {
    albums: 1,
    artists: 1,
    tracks: 1,
};

/// How long the decode probe listens before judging playback.
const DECODE_WINDOW: Duration = Duration::from_millis(2200);
/// How far playback must have got inside that window.
const DECODE_MIN_POS: Duration = Duration::from_millis(800);
/// How long to wait for the position to settle and advance after a seek.
const SEEK_WINDOW: Duration = Duration::from_millis(2500);
/// Where the seek probe aims, as a fraction of the track.
const SEEK_FRACTION: f32 = 0.25;
/// How much of a song's title the add-songs filter probe types.
const FILTER_CHARS: usize = 3;

/// Run every check. Returns the process exit code.
pub fn run() -> i32 {
    let mut report = Report::default();

    // Resolved exactly as the app resolves them, so `PHOEBUS_DATA` / `PHOEBUS_LIBRARY` move
    // the whole self-test off the real home together.
    let dirs = Dirs::resolve();
    let state = AppState::load_from(&dirs.state_path());
    let root = phoebus_core::library_root_for(state.configured_library_root());
    println!(
        "-- phoebus selftest, library root {} (read-only), app data {}",
        root.display(),
        dirs.data_dir().display()
    );
    let raw = std::env::var(ENV_EXPECT).ok();
    let expect = match raw.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(text) => match Expect::parse(text) {
            Ok(expect) => {
                println!("-- scan minimums {expect} (from {ENV_EXPECT}={text:?})");
                expect
            }
            Err(e) => {
                println!("-- scan minimums {DEFAULT_EXPECT} (default)");
                report.check(false, "selftest.expect", &format!("{ENV_EXPECT}: {e}"));
                DEFAULT_EXPECT
            }
        },
        None => {
            println!(
                "-- scan minimums {DEFAULT_EXPECT} (default; set {ENV_EXPECT}=\"albums,artists,tracks\" to demand more)"
            );
            DEFAULT_EXPECT
        }
    };
    // Covers go to the active data dir and nowhere else — the library root is never written.
    let library = scanner::scan_with_covers(&root, &dirs.covers_dir());

    check_scan(&mut report, &library, expect);
    check_albums(&mut report, &library);
    check_audio(&mut report, &library);
    check_playlists(&mut report, &library);
    check_reorder(&mut report, &library);
    check_add_songs(&mut report, &library);
    check_favorites(&mut report, &library);
    check_state(&mut report);

    println!("-- {} checks, {} failed", report.total, report.failures);
    i32::from(report.failures > 0)
}

// ---------------------------------------------------------------------------------------
// Library
// ---------------------------------------------------------------------------------------

/// Minimum library shape the scan checks demand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Expect {
    albums: usize,
    artists: usize,
    tracks: usize,
}

impl Expect {
    /// Parse `albums,artists,tracks`.
    fn parse(text: &str) -> Result<Expect, String> {
        let mut counts = Vec::new();
        for field in text.split(',') {
            let field = field.trim();
            let n: usize = field
                .parse()
                .map_err(|_| format!("{field:?} is not a whole number"))?;
            counts.push(n);
        }
        match counts[..] {
            [albums, artists, tracks] => Ok(Expect {
                albums,
                artists,
                tracks,
            }),
            _ => Err(format!(
                "expected three comma-separated numbers (albums,artists,tracks), got {}",
                counts.len()
            )),
        }
    }
}

impl std::fmt::Display for Expect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            ">= {} album(s), >= {} artist(s), >= {} track(s)",
            self.albums, self.artists, self.tracks
        )
    }
}

fn check_scan(report: &mut Report, library: &Library, expect: Expect) {
    report.check(
        library.album_count() >= expect.albums,
        "scan.albums",
        &format!(
            "{} albums (need >= {})",
            library.album_count(),
            expect.albums
        ),
    );
    report.check(
        library.artist_count() >= expect.artists,
        "scan.artists",
        &format!(
            "{} artists (need >= {})",
            library.artist_count(),
            expect.artists
        ),
    );
    report.check(
        library.track_count() >= expect.tracks,
        "scan.tracks",
        &format!(
            "{} tracks (need >= {})",
            library.track_count(),
            expect.tracks
        ),
    );
}

fn check_albums(report: &mut Report, library: &Library) {
    for key in library.albums() {
        let Some(album) = library.album(key) else {
            report.check(false, "album.present", &format!("{key} vanished mid-scan"));
            continue;
        };
        let label = format!("{} — {}", album.artist, album.title);

        let untagged = album
            .track_ids
            .iter()
            .filter(|id| !is_tagged(library, **id))
            .count();
        report.check(
            untagged == 0 && !album.track_ids.is_empty(),
            "album.tags",
            &format!(
                "{label}: {} tracks, {untagged} missing title/artist/album",
                album.track_count()
            ),
        );

        let zero = album
            .track_ids
            .iter()
            .filter(|id| library.track(**id).is_none_or(|t| t.duration.is_zero()))
            .count();
        report.check(
            zero == 0 && !album.duration.is_zero(),
            "album.duration",
            &format!(
                "{label}: {}s total, {zero} tracks without a duration",
                album.duration.as_secs()
            ),
        );

        // Only albums with embedded artwork ever get a cache PNG; an art-less album is
        // not a failure, it is just reported.
        if album.has_artwork {
            let cover = library.cover_path(key);
            let bytes = std::fs::metadata(&cover).map(|m| m.len()).unwrap_or(0);
            report.check(
                bytes > 0,
                "album.artwork",
                &format!("{label}: {bytes} bytes at {}", cover.display()),
            );
        } else {
            println!("INFO album.artwork {label}: no embedded artwork, cache not expected");
        }
    }
}

/// A track counts as tagged when the scanner ended up with SOME non-empty title, artist
/// and album. Placeholder names are fine: Apple Music itself files untagged imports under
/// literal `Unknown Artist/Unknown Album` folders, and the path fallback adopting those
/// names is the scanner doing its job, not a failure.
fn is_tagged(library: &Library, id: TrackId) -> bool {
    let Some(track) = library.track(id) else {
        return false;
    };
    !track.title.trim().is_empty()
        && !track.artist.trim().is_empty()
        && !track.album.trim().is_empty()
        && !track.album_artist.trim().is_empty()
}

// ---------------------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------------------

fn check_audio(report: &mut Report, library: &Library) {
    let Some(probe) = first_track(library) else {
        report.check(false, "audio.decode", "the library has no track to probe");
        report.check(false, "audio.seek", "the library has no track to probe");
        return;
    };
    let (path, duration) = probe;

    let player = match PlayerHandle::spawn() {
        Ok(p) => p,
        Err(e) => {
            let detail = format!("no audio device: {e:#}");
            report.check(false, "audio.decode", &detail);
            report.check(false, "audio.seek", &detail);
            return;
        }
    };
    // Everything below runs silent: the probe must never be audible.
    if let Err(e) = player.set_volume(0.0) {
        report.check(false, "audio.decode", &format!("volume: {e:#}"));
        return;
    }
    // One load, one generation: every event below must carry it, and anything that does
    // not belongs to a track this probe never asked for.
    const GENERATION: u64 = 1;
    if let Err(e) = player.load(&path, true, GENERATION) {
        report.check(false, "audio.decode", &format!("load: {e:#}"));
        return;
    }

    let mut reported = None;
    let mut seekable = false;
    let mut furthest = Duration::ZERO;
    let mut failure = None;
    let mut stale = 0usize;
    pump(&player, DECODE_WINDOW, |event| {
        if event.generation != GENERATION {
            stale += 1;
            return true;
        }
        match event.kind {
            EventKind::Loaded {
                duration,
                seekable: can_seek,
            } => {
                reported = Some(duration);
                seekable = can_seek;
                true
            }
            EventKind::Progress { pos } => {
                furthest = furthest.max(pos);
                true
            }
            EventKind::Ended => false,
            EventKind::SeekFailed { message, .. } | EventKind::Error(message) => {
                failure = Some(message);
                false
            }
        }
    });
    report.check(
        failure.is_none() && furthest >= DECODE_MIN_POS && stale == 0,
        "audio.decode",
        &format!(
            "{} played to {:.2}s at volume 0 ({stale} events from another generation){}",
            file_label(&path),
            furthest.as_secs_f32(),
            failure
                .as_ref()
                .map_or_else(String::new, |m| format!(" (error: {m})"))
        ),
    );

    let total = reported.unwrap_or(duration);
    let target = total.mul_f32(SEEK_FRACTION);
    if let Err(e) = player.seek_to(target) {
        report.check(false, "audio.seek", &format!("seek: {e:#}"));
        return;
    }
    let mut landed = None;
    let mut advanced = None;
    let mut refused = None;
    pump(&player, SEEK_WINDOW, |event| {
        if event.generation != GENERATION {
            return true;
        }
        match event.kind {
            EventKind::Progress { pos } => {
                if pos + Duration::from_millis(500) < target {
                    return true; // a stale pre-seek tick
                }
                match landed {
                    None => landed = Some(pos),
                    Some(first) if pos > first => {
                        advanced = Some(pos);
                        return false;
                    }
                    Some(_) => {}
                }
                true
            }
            EventKind::SeekFailed { message, .. } => {
                refused = Some(message);
                false
            }
            EventKind::Error(_) | EventKind::Ended => false,
            EventKind::Loaded { .. } => true,
        }
    });
    let ok = landed.is_some_and(|p| p + Duration::from_millis(500) >= target)
        && advanced.is_some()
        && refused.is_none();
    report.check(
        ok,
        "audio.seek",
        &format!(
            "target {:.2}s, landed at {:.2}s, advanced to {:.2}s (seekable {seekable}{})",
            target.as_secs_f32(),
            landed.unwrap_or_default().as_secs_f32(),
            advanced.unwrap_or_default().as_secs_f32(),
            refused
                .as_ref()
                .map_or_else(String::new, |m| format!(", refused: {m}"))
        ),
    );
}

/// Drain engine events for at most `window`. `on_event` returns `false` to stop early.
fn pump(player: &PlayerHandle, window: Duration, mut on_event: impl FnMut(Event) -> bool) {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        match player.events().recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                if !on_event(event) {
                    return;
                }
            }
            Err(_) => continue,
        }
    }
}

fn first_track(library: &Library) -> Option<(PathBuf, Duration)> {
    let id = *library.tracks_sorted().first()?;
    let track = library.track(id)?;
    Some((library.track_path(id)?, track.duration))
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

// ---------------------------------------------------------------------------------------
// Persistence (temporary directories only)
// ---------------------------------------------------------------------------------------

fn check_playlists(report: &mut Report, library: &Library) {
    let dir = match scratch("playlists") {
        Ok(dir) => dir,
        Err(e) => {
            report.check(false, "playlists.roundtrip", &e);
            return;
        }
    };
    let ids: Vec<TrackId> = library.tracks_sorted().iter().copied().take(3).collect();
    let path = Dirs::at(&dir).playlists_path();

    let mut store = PlaylistStore::load_from(&path);
    let outcome = store
        .create(Some("Selftest"))
        .and_then(|id| store.append_tracks(id, library, &ids).map(|()| id));
    let id = match outcome {
        Ok(id) => id,
        Err(e) => {
            report.check(false, "playlists.roundtrip", &format!("create: {e:#}"));
            cleanup(&dir);
            return;
        }
    };

    let reloaded = PlaylistStore::load_from(&path);
    let entry = reloaded.get(id);
    let name_ok = entry.is_some_and(|p| p.name == "Selftest");
    let entries_ok = entry.is_some_and(|p| p.entries.len() == ids.len());
    let resolve_ok = reloaded.resolve(id, library) == ids;

    let mut store = reloaded;
    let deleted = store.delete(id).is_ok() && PlaylistStore::load_from(&path).get(id).is_none();

    report.check(
        name_ok && entries_ok && resolve_ok && deleted,
        "playlists.roundtrip",
        &format!(
            "create+save+reload+verify+delete of {} entries in {} (name {name_ok}, entries {entries_ok}, resolve {resolve_ok}, delete {deleted})",
            ids.len(),
            path.display()
        ),
    );
    cleanup(&dir);
}

/// Drag the last song of a real playlist to the top, and check that the new order is on
/// disk — the whole of the playlist reorder gesture below the pointer.
///
/// The two calls are exactly the ones the view makes on a drop: `move_target` turns the gap
/// the row was released into (`0` — above everything) into a `move_entry` index, and
/// `move_entry` writes. What this adds over the unit tests is the reload: a reorder that
/// only happened in memory looks perfect until the app is restarted, which is precisely the
/// failure a headless check can catch and a screenshot cannot.
fn check_reorder(report: &mut Report, library: &Library) {
    let dir = match scratch("reorder") {
        Ok(dir) => dir,
        Err(e) => {
            report.check(false, "playlists.reorder", &e);
            return;
        }
    };
    let ids: Vec<TrackId> = library.tracks_sorted().iter().copied().take(3).collect();
    if ids.len() < 2 {
        report.check(false, "playlists.reorder", "the library has too few tracks");
        cleanup(&dir);
        return;
    }
    let path = Dirs::at(&dir).playlists_path();
    let mut store = PlaylistStore::load_from(&path);
    let outcome = store
        .create(Some("Reorder"))
        .and_then(|id| store.append_tracks(id, library, &ids).map(|()| id));
    let id = match outcome {
        Ok(id) => id,
        Err(e) => {
            report.check(false, "playlists.reorder", &format!("create: {e:#}"));
            cleanup(&dir);
            return;
        }
    };

    let last = ids.len() - 1;
    let moved = phoebus_core::playlists::move_target(last, 0)
        .ok_or_else(|| "the top gap was reported as a no-op".to_string())
        .and_then(|to| store.move_entry(id, last, to).map_err(|e| format!("{e:#}")));
    let mut want: Vec<TrackId> = ids.clone();
    want.rotate_right(1);
    let reloaded = PlaylistStore::load_from(&path).resolve(id, library) == want;

    // …and the drop that goes nowhere writes nowhere: `move_target` refuses both gaps that
    // touch the dragged row, which is what keeps an aimless drag out of `playlists.json`.
    let no_op = phoebus_core::playlists::move_target(0, 0).is_none()
        && phoebus_core::playlists::move_target(0, 1).is_none();

    report.check(
        moved.is_ok() && reloaded && no_op,
        "playlists.reorder",
        &match moved {
            Ok(()) => format!(
                "dragged song {last} to the top of {} and reloaded it (order {reloaded}, no-op guard {no_op})",
                path.display()
            ),
            Err(e) => format!("move: {e}"),
        },
    );
    cleanup(&dir);
}

/// Walk the add-songs picker's path over the real library, with no window
/// (UI-SPEC v1.4 §Add songs).
///
/// The popup is three calls in a trench coat, and this runs all three against the songs the
/// user actually has: `filter_tracks` decides which rows are on screen, `Playlist::contains`
/// decides whether each of them shows `+` or `✓`, and `append_tracks` is what the `+` ends
/// up calling. The interesting failure is the last two disagreeing — an add that lands but
/// leaves the row still offering `+`, which is how a playlist quietly fills with duplicates.
///
/// Against a **temporary** data directory, like the two checks around it: this one writes.
fn check_add_songs(report: &mut Report, library: &Library) {
    let dir = match scratch("add-songs") {
        Ok(dir) => dir,
        Err(e) => {
            report.check(false, "playlists.add_song", &e);
            return;
        }
    };
    let Some(&id) = library.tracks_sorted().first() else {
        report.check(false, "playlists.add_song", "the library has no tracks");
        cleanup(&dir);
        return;
    };

    // An empty filter is the whole library, in the Songs view's order.
    let all_ok = phoebus_core::filter_tracks(library, "") == library.tracks_sorted();
    // …and typing the head of a title has to keep that title's own row.
    let title: String = library
        .track(id)
        .map(|t| t.title.clone())
        .unwrap_or_default();
    let needle: String = title.chars().take(FILTER_CHARS).collect();
    let hits = phoebus_core::filter_tracks(library, &needle);
    let filter_ok = needle.trim().is_empty() || hits.contains(&id);

    let path = Dirs::at(&dir).playlists_path();
    let mut store = PlaylistStore::load_from(&path);
    let outcome = store.create(Some("Selftest Add")).and_then(|pl| {
        let before = store.get(pl).is_some_and(|p| !p.contains(id));
        store
            .append_tracks(pl, library, &[id])
            .map(|()| (pl, before))
    });
    let (pl, before_ok) = match outcome {
        Ok(pair) => pair,
        Err(e) => {
            report.check(false, "playlists.add_song", &format!("add: {e:#}"));
            cleanup(&dir);
            return;
        }
    };

    let after_ok = store.get(pl).is_some_and(|p| p.contains(id));
    // Reopening the popup after a restart must still show the checkmark.
    let reloaded = PlaylistStore::load_from(&path);
    let persisted_ok = reloaded
        .get(pl)
        .is_some_and(|p| p.contains(id) && p.entries.len() == 1);

    let mut store = reloaded;
    let deleted = store.delete(pl).is_ok();

    report.check(
        all_ok && filter_ok && before_ok && after_ok && persisted_ok && deleted,
        "playlists.add_song",
        &format!(
            "filter {:?} -> {} of {} songs, then add+reload in {} (unfiltered {all_ok}, filter {filter_ok}, before {before_ok}, after {after_ok}, persisted {persisted_ok}, delete {deleted})",
            needle,
            hits.len(),
            library.track_count(),
            path.display()
        ),
    );
    cleanup(&dir);
}

/// Heart an album and some songs, reload the file, and check every answer survived
/// (UI-SPEC v1.3 §Favorites).
///
/// Against a **temporary** data directory, like the playlist check beside it: the whole
/// point of a favourites round-trip is that it writes, and the user's own `favorites.json`
/// is not this program's to touch.
fn check_favorites(report: &mut Report, library: &Library) {
    let dir = match scratch("favorites") {
        Ok(dir) => dir,
        Err(e) => {
            report.check(false, "favorites.roundtrip", &e);
            return;
        }
    };
    let path = Dirs::at(&dir).favorites_path();
    let ids: Vec<TrackId> = library.tracks_sorted().iter().copied().take(3).collect();
    let album = library.albums().first().cloned();

    let mut favs = Favorites::load_from(&path);
    let fresh = favs.is_empty() && !favs.is_read_only();
    for id in &ids {
        favs.toggle_track(library, *id);
    }
    if let Some(key) = &album {
        favs.toggle_album(key);
    }
    let saved = favs.save().is_ok() && path.is_file();

    // The reload is the whole check: ids are hashes of the *paths* in the file, so a
    // favourite that comes back wrong means the file, the hashing or the resolve is wrong.
    let mut reloaded = Favorites::load_from(&path);
    reloaded.resolve(library);
    let tracks_ok = ids.iter().all(|id| reloaded.is_track(*id));
    let album_ok = album.as_ref().is_none_or(|key| reloaded.is_album(key));
    // …in `tracks_sorted` order, which is what the Favorites view and its PLAY rely on.
    let order_ok = reloaded.track_ids(library) == ids;

    // And unhearting has to reach the file too, or a removed favourite comes back.
    for id in &ids {
        reloaded.toggle_track(library, *id);
    }
    if let Some(key) = &album {
        reloaded.toggle_album(key);
    }
    let cleared = Favorites::load_from(&path).is_empty();

    report.check(
        fresh && saved && tracks_ok && album_ok && order_ok && cleared,
        "favorites.roundtrip",
        &format!(
            "heart+save+reload+resolve+unheart of {} track(s) and {} album(s) in {} (fresh {fresh}, saved {saved}, tracks {tracks_ok}, album {album_ok}, order {order_ok}, cleared {cleared})",
            ids.len(),
            usize::from(album.is_some()),
            path.display()
        ),
    );
    cleanup(&dir);
}

fn check_state(report: &mut Report) {
    let dir = match scratch("state") {
        Ok(dir) => dir,
        Err(e) => {
            report.check(false, "state.roundtrip", &e);
            return;
        }
    };
    let path = Dirs::at(&dir).state_path();
    let written = AppState {
        volume: 0.35,
        shuffle: true,
        repeat: Repeat::One,
        last_view: "settings".to_string(),
        window: Some((1440.0, 900.0)),
        library_root: Some("~/Music/Media.localized/Music".to_string()),
        theme_mode: phoebus_core::ThemeMode::Light,
        accent: "#2EF0FF".to_string(),
        // Three dragged-looking widths, all inside their ranges, so the round trip proves
        // the v1.4 panel widths survive a save/load and not just their defaults.
        sidebar_w: 265.0,
        queue_w: 244.0,
        artist_list_w: 310.0,
    };
    let saved = written.save_to(&path);
    let read_back = AppState::load_from(&path);
    report.check(
        saved.is_ok() && read_back == written,
        "state.roundtrip",
        &format!(
            "{} (saved {}, matches {})",
            path.display(),
            saved.is_ok(),
            read_back == written
        ),
    );
    cleanup(&dir);
}

/// A fresh directory under the system temp dir — never the user's library.
fn scratch(tag: &str) -> Result<PathBuf, String> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!(
        "phoebus-selftest-{tag}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    Ok(dir)
}

fn cleanup(dir: &Path) {
    if let Err(e) = std::fs::remove_dir_all(dir) {
        log::debug!("selftest: could not remove {}: {e}", dir.display());
    }
}

// ---------------------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------------------

#[derive(Default)]
struct Report {
    total: usize,
    failures: usize,
}

impl Report {
    fn check(&mut self, ok: bool, name: &str, detail: &str) {
        self.total += 1;
        if ok {
            println!("PASS {name} {detail}");
        } else {
            self.failures += 1;
            println!("FAIL {name} {detail}");
        }
    }
}
