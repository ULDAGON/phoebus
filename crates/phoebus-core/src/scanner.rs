//! Filesystem scan: walk the library root, read tags with `lofty`, build a [`Library`],
//! and refresh the per-album cover cache.
//!
//! Robustness rule: a file that cannot be parsed is skipped with a `log::warn!`. The
//! scanner never panics and never returns an error — a broken library still yields whatever
//! could be read.

use std::ffi::OsStr;
use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::time::SystemTime;

use anyhow::{Context, Result};
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::{Accessor, ItemKey};
use lofty::tag::Tag;
use walkdir::{DirEntry, WalkDir};

use crate::model::{Library, Track};
use crate::paths;

/// Extensions the scanner picks up (matched case-insensitively).
pub const AUDIO_EXTENSIONS: &[&str] = &["m4a", "mp3", "flac", "ogg", "wav", "aiff", "aac"];

/// Deepest directory nesting the walker descends into.
const MAX_DEPTH: usize = 8;

/// Longest edge of a cached cover PNG, in pixels.
pub const COVER_MAX_EDGE: u32 = 600;

/// Which stage of the scan a [`ScanProgress`] belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScanPhase {
    /// Walking directories, before any file has been read.
    Discovering,
    /// Reading tags, one audio file at a time.
    Reading,
    /// Extracting and resizing album covers.
    Artwork,
    /// Finished.
    Done,
}

/// A progress tick handed to the callback of [`scan_with_progress`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanProgress {
    /// Current stage.
    pub phase: ScanPhase,
    /// Items completed in this stage.
    pub done: usize,
    /// Items in this stage, when it is known up front.
    pub total: Option<usize>,
    /// Tracks accepted so far.
    pub tracks: usize,
    /// What is being worked on (relative path, or album title during the artwork phase).
    pub current: Option<String>,
}

/// Scan `root` into a self-contained [`Library`], caching covers in the data directory that
/// belongs to `root` ([`Dirs::inside`](crate::paths::Dirs::inside), `<root>/.phoebus/cache/covers`).
///
/// The app never uses this — it resolves its data directory independently and calls
/// [`scan_with_covers`], so a configured library root is only ever read from. This shape is
/// for tests and probes that own the directory they scan.
///
/// Never panics; unreadable files are skipped.
pub fn scan(root: &Path) -> Library {
    scan_with_progress(root, |_| {})
}

/// [`scan`] with a progress callback. The callback runs on the calling thread (the app runs
/// the whole scan on a background thread and forwards ticks over a channel).
pub fn scan_with_progress<F: FnMut(ScanProgress)>(root: &Path, progress: F) -> Library {
    scan_with_covers_progress(root, &paths::Dirs::inside(root).covers_dir(), progress)
}

/// Scan `root` into a [`Library`] whose cover cache lives at `covers_dir`.
///
/// This is the v1.1 shape: `covers_dir` is [`Dirs::covers_dir`](crate::paths::Dirs::covers_dir),
/// outside the library root, so a scan of someone's Apple Music folder writes nothing into
/// it.
pub fn scan_with_covers(root: &Path, covers_dir: &Path) -> Library {
    scan_with_covers_progress(root, covers_dir, |_| {})
}

/// [`scan_with_covers`] with a progress callback — the one function the other three
/// delegate to.
///
/// The walk skips every dot-entry (so `<root>/.phoebus` is invisible whatever `covers_dir`
/// is) and additionally prunes `covers_dir` itself, so pointing the cache at a plainly-named
/// directory inside the library cannot feed the cache back into the library.
pub fn scan_with_covers_progress<F: FnMut(ScanProgress)>(
    root: &Path,
    covers_dir: &Path,
    mut progress: F,
) -> Library {
    progress(ScanProgress {
        phase: ScanPhase::Discovering,
        done: 0,
        total: None,
        tracks: 0,
        current: None,
    });

    let mut tracks: Vec<Track> = Vec::new();
    let mut files_seen = 0usize;

    let walker = WalkDir::new(root)
        .max_depth(MAX_DEPTH)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !is_hidden(e) && e.path() != covers_dir);

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                log::warn!("scan: skipping unreadable entry: {e}");
                continue;
            }
        };
        if !entry.file_type().is_file() || !has_audio_extension(entry.path()) {
            continue;
        }
        files_seen += 1;
        let rel = match read_track(root, entry.path()) {
            Some(track) => {
                let rel = track.rel_path.clone();
                tracks.push(track);
                Some(rel)
            }
            None => None,
        };
        progress(ScanProgress {
            phase: ScanPhase::Reading,
            done: files_seen,
            total: None,
            tracks: tracks.len(),
            current: rel,
        });
    }

    let library = Library::build_with_covers(root.to_path_buf(), covers_dir.to_path_buf(), tracks);
    refresh_covers(root, covers_dir, &library, &mut progress);

    progress(ScanProgress {
        phase: ScanPhase::Done,
        done: library.track_count(),
        total: Some(library.track_count()),
        tracks: library.track_count(),
        current: None,
    });
    library
}

/// Dot-files and dot-directories are skipped — including `<root>/.phoebus`, the app's own
/// data directory. The root itself is always accepted (the root *is* `~/.phoebus`).
fn is_hidden(entry: &DirEntry) -> bool {
    entry.depth() > 0 && name_is_hidden(entry.file_name())
}

/// True only for names that really do start with a `.`.
///
/// This is the `filter_entry` predicate, so a `true` here prunes a whole subtree. Only the
/// first *byte* is inspected: ext4/xfs/btrfs accept any byte sequence in a name, and
/// treating "not valid UTF-8" as "hidden" silently deleted whole artists from the library
/// (a Latin-1 `Björk/` directory and everything under it).
fn name_is_hidden(name: &OsStr) -> bool {
    name.as_encoded_bytes().first() == Some(&b'.')
}

fn has_audio_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|ext| AUDIO_EXTENSIONS.contains(&ext.as_str()))
}

/// Read one file into a [`Track`], or `None` if it is not readable as audio.
///
/// Every field falls back to something derived from the path, so an entirely untagged
/// `Artist/Album/01 Title.mp3` still lands in the right album.
fn read_track(root: &Path, path: &Path) -> Option<Track> {
    let rel = paths::rel_path_string(root, path)?;
    let mut track = Track::new(&rel);
    // `rel_path_string` renders the path lossily, which only bites when a name is not valid
    // UTF-8 (legal on Linux file systems). Keep the real path so the engine still opens the
    // right file, and say so once, because the displayed name is then approximate.
    if root.join(&track.rel_path) != path {
        log::warn!(
            "scan: {} has a name that is not valid UTF-8; listing it as {}",
            path.display(),
            track.rel_path
        );
        track.source_path = Some(path.to_path_buf());
    }
    track.added_at = fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or_else(|_| SystemTime::now());

    let tagged = match lofty::read_from_path(path) {
        Ok(t) => t,
        Err(e) => {
            log::warn!("scan: skipping {}: {e}", path.display());
            return None;
        }
    };
    track.duration = tagged.properties().duration();

    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        if let Some(v) = text(tag.title().as_deref()) {
            track.title = v;
        }
        if let Some(v) = text(tag.artist().as_deref()) {
            track.artist = v;
        }
        if let Some(v) = text(tag.album().as_deref()) {
            track.album = v;
        }
        // album_artist <- aART <- artist <- grandparent directory. The seeded library has
        // no aART atom at all, so without this every album collapses into one key.
        track.album_artist =
            text(tag.get_string(ItemKey::AlbumArtist)).unwrap_or_else(|| track.artist.clone());
        track.genre = text(tag.genre().as_deref());
        if let Some(n) = tag.track() {
            track.track_no = Some(n);
        }
        track.disc_no = tag.disk();
        track.year = tag
            .date()
            .map(|d| u32::from(d.year))
            .or_else(|| text(tag.get_string(ItemKey::RecordingDate)).and_then(|s| parse_year(&s)))
            .or_else(|| text(tag.get_string(ItemKey::Year)).and_then(|s| parse_year(&s)));
        // Never filter by PictureType: real files tag their cover as `Other`.
        track.has_artwork = !tag.pictures().is_empty();
    }

    track.refresh_key();
    Some(track)
}

fn text(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Pull a 4-digit year out of a date string like `2021` or `2021-06-01`.
fn parse_year(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|y| (1000..=9999).contains(y))
}

/// Bring the cover cache in line with the artwork that is actually in the files.
///
/// The cache file name is `fnv1a(album_key)`, which does not change when the *art* does, so
/// existence alone is not freshness: a cover older than the track it came from is redone,
/// and an album that lost its artwork loses its cached PNG.
///
/// `covers_dir` is the only directory this creates or writes to — when it is outside `root`
/// (the v1.1 layout) the library root is never touched.
fn refresh_covers<F: FnMut(ScanProgress)>(
    root: &Path,
    covers_dir: &Path,
    library: &Library,
    progress: &mut F,
) {
    if library.album_count() == 0 {
        return;
    }
    if let Err(e) = fs::create_dir_all(covers_dir) {
        log::warn!("scan: no cover cache at {} ({e})", covers_dir.display());
        return;
    }

    let total = library.album_count();
    for (i, key) in library.albums().iter().enumerate() {
        let Some(album) = library.album(key) else {
            continue;
        };
        progress(ScanProgress {
            phase: ScanPhase::Artwork,
            done: i + 1,
            total: Some(total),
            tracks: library.track_count(),
            current: Some(album.title.clone()),
        });
        let dest = album.cover_path(covers_dir);
        let source = album
            .track_ids
            .iter()
            .filter_map(|id| library.track(*id))
            .find(|t| t.has_artwork);
        let Some(source) = source else {
            // The album lost its artwork since the last scan. The cache name is derived
            // from the album key, not from the art, so a leftover PNG would keep being
            // painted forever.
            if dest.exists()
                && let Err(e) = fs::remove_file(&dest)
            {
                log::warn!("scan: stale cover for {} stayed behind: {e}", album.title);
            }
            continue;
        };
        if is_cover_current(&dest, source.added_at) {
            continue;
        }
        let source = source.abs_path(root);
        match first_picture(&source).map(|data| write_cover_png(&data, &dest)) {
            Some(Ok(())) => {}
            Some(Err(e)) => log::warn!("scan: cover for {} failed: {e:#}", album.title),
            None => log::warn!("scan: no readable picture in {}", source.display()),
        }
    }
}

/// True when the cached cover is at least as new as the file it was extracted from.
///
/// One `stat` per album. A missing or unreadable cache file is "not current", so the normal
/// first-scan path is unchanged; a re-tagged track is newer than its cover and gets a fresh
/// one.
fn is_cover_current(dest: &Path, source_mtime: SystemTime) -> bool {
    fs::metadata(dest)
        .and_then(|m| m.modified())
        .is_ok_and(|cached| cached >= source_mtime)
}

/// The first embedded picture of a file, whatever its `PictureType`.
fn first_picture(path: &Path) -> Option<Vec<u8>> {
    let tagged = lofty::read_from_path(path).ok()?;
    let tag: &Tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    let pic = tag.pictures().first()?;
    Some(pic.data().to_vec())
}

/// Decode `data`, shrink it to at most [`COVER_MAX_EDGE`] on its longest edge with Lanczos3,
/// and write it to `dest` as a PNG (atomically, so a half-written cover never appears).
pub(crate) fn write_cover_png(data: &[u8], dest: &Path) -> Result<()> {
    let img = image::load_from_memory(data).context("decoding embedded artwork")?;
    let img = if img.width() > COVER_MAX_EDGE || img.height() > COVER_MAX_EDGE {
        img.resize(
            COVER_MAX_EDGE,
            COVER_MAX_EDGE,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        img
    };
    let mut buf: Vec<u8> = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
        .context("encoding cover PNG")?;
    paths::write_atomic(dest, &buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AlbumKey, TrackId};
    use std::path::PathBuf;
    use std::time::Duration;

    /// Byte-by-byte minimal 8 kHz mono 16-bit PCM WAV — a real file lofty can parse, with
    /// no tags at all, so every path fallback is exercised.
    fn write_wav(path: &Path, millis: u32) {
        let sample_rate: u32 = 8000;
        let samples = sample_rate * millis / 1000;
        let data_len = samples * 2;
        let mut b: Vec<u8> = Vec::with_capacity(44 + data_len as usize);
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
        b.extend_from_slice(&1u16.to_le_bytes()); // PCM
        b.extend_from_slice(&1u16.to_le_bytes()); // mono
        b.extend_from_slice(&sample_rate.to_le_bytes());
        b.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
        b.extend_from_slice(&2u16.to_le_bytes()); // block align
        b.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        b.resize(44 + data_len as usize, 0);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, &b).expect("write wav");
    }

    /// A square PNG of `edge` px, used both as embedded artwork and as a fake cache entry.
    fn png_bytes(edge: u32) -> Vec<u8> {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(edge, edge, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 0])
        }));
        let mut out: Vec<u8> = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("encode png");
        out
    }

    /// A WAV plus an ID3v2 tag carrying one embedded picture — the scanner's artwork path.
    fn write_wav_with_cover(path: &Path, millis: u32, edge: u32) {
        use lofty::config::WriteOptions;
        use lofty::picture::{MimeType, Picture, PictureType};
        use lofty::tag::{Tag, TagExt, TagType};

        write_wav(path, millis);
        let mut tag = Tag::new(TagType::Id3v2);
        tag.push_picture(
            Picture::unchecked(png_bytes(edge))
                .mime_type(MimeType::Png)
                .pic_type(PictureType::Other)
                .build(),
        );
        tag.save_to_path(path, WriteOptions::default())
            .expect("embed cover");
    }

    fn set_mtime(path: &Path, at: SystemTime) {
        fs::File::options()
            .write(true)
            .open(path)
            .expect("open")
            .set_modified(at)
            .expect("set mtime");
    }

    fn seeded_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write_wav(&root.join("HOME/Odyssey/01 Intro.wav"), 300);
        write_wav(&root.join("HOME/Odyssey/02 - Resonance.wav"), 400);
        write_wav(&root.join("Woodkid/S16/01 Goliath.wav"), 500);
        // Dot entries everywhere: the app data dir, a hidden folder, a hidden file.
        write_wav(&root.join(".phoebus/cache/covers/sneaky.wav"), 100);
        write_wav(&root.join(".hidden/nope.wav"), 100);
        write_wav(&root.join("HOME/Odyssey/.hidden.wav"), 100);
        // Non-audio and unparseable files.
        fs::write(root.join("HOME/Odyssey/cover.jpg"), b"not audio").expect("write");
        fs::write(root.join("HOME/Odyssey/03 Broken.wav"), b"NOT A WAV AT ALL").expect("write");
        dir
    }

    #[test]
    fn scan_applies_every_path_fallback() {
        let dir = seeded_root();
        let lib = scan(dir.path());
        assert_eq!(lib.track_count(), 3, "3 real tracks, corrupt one skipped");

        let id = TrackId::for_rel_path("HOME/Odyssey/01 Intro.wav");
        let t = lib.track(id).expect("track present");
        assert_eq!(t.title, "Intro", "title <- file stem minus NN prefix");
        assert_eq!(t.artist, "HOME", "artist <- grandparent dir");
        assert_eq!(t.album_artist, "HOME", "album_artist <- artist");
        assert_eq!(t.album, "Odyssey", "album <- parent dir");
        assert_eq!(t.track_no, Some(1), "track_no <- NN prefix");
        assert!(!t.has_artwork);
        assert!(t.duration.as_millis() >= 250, "duration from properties");
        assert!(t.added_at > SystemTime::UNIX_EPOCH, "added_at from mtime");

        let t2 = lib
            .track(TrackId::for_rel_path("HOME/Odyssey/02 - Resonance.wav"))
            .expect("track present");
        assert_eq!(t2.title, "Resonance");
        assert_eq!(t2.track_no, Some(2));
    }

    #[test]
    fn scan_groups_albums_without_any_album_artist_tag() {
        let dir = seeded_root();
        let lib = scan(dir.path());
        assert_eq!(
            lib.album_count(),
            2,
            "albums must not collapse into one key"
        );
        assert_eq!(lib.artist_count(), 2);
        let odyssey = lib
            .album(&AlbumKey::new("HOME", "Odyssey"))
            .expect("album present");
        assert_eq!(odyssey.track_ids.len(), 2);
        assert_eq!(odyssey.artist, "HOME");
        assert!(!odyssey.has_artwork);
        assert_eq!(lib.albums_of_artist("home").len(), 1);
    }

    #[test]
    fn scan_skips_dot_entries_including_the_app_data_dir() {
        let dir = seeded_root();
        let lib = scan(dir.path());
        for t in lib.tracks_sorted() {
            let rel = &lib.track(*t).expect("track").rel_path;
            assert!(
                !rel.split('/').any(|c| c.starts_with('.')),
                "dot entry leaked into the library: {rel}"
            );
        }
        assert!(
            lib.track(TrackId::for_rel_path(".hidden/nope.wav"))
                .is_none()
        );
    }

    /// A backslash is legal in a POSIX file name. Such a track must land under the right
    /// artist/album *and* its absolute path must be the file that is actually on disk.
    #[test]
    fn scan_keeps_backslashes_in_directory_and_file_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write_wav(&root.join("AC\\DC/Back in Black/01 Hells Bells.wav"), 300);
        write_wav(&root.join("Normal Band/Album/02 Bad\\Ass.wav"), 300);
        let lib = scan(root);
        assert_eq!(lib.track_count(), 2);

        let id = TrackId::for_rel_path("AC\\DC/Back in Black/01 Hells Bells.wav");
        let t = lib.track(id).expect("the AC\\DC track is in the library");
        assert_eq!(t.rel_path, "AC\\DC/Back in Black/01 Hells Bells.wav");
        assert_eq!(t.artist, "AC\\DC", "one directory, not two");
        assert_eq!(t.album, "Back in Black");
        assert_eq!(t.title, "Hells Bells");
        let abs = lib.track_path(id).expect("path");
        assert!(
            abs.exists(),
            "the engine must be handed a real file: {abs:?}"
        );

        let id2 = TrackId::for_rel_path("Normal Band/Album/02 Bad\\Ass.wav");
        let t2 = lib
            .track(id2)
            .expect("the backslashed file name is in the library");
        assert_eq!(t2.artist, "Normal Band");
        assert_eq!(t2.album, "Album");
        assert_eq!(t2.title, "Bad\\Ass", "no phantom album from a file name");
        assert_eq!(t2.track_no, Some(2));
        assert!(lib.track_path(id2).expect("path").exists());
        assert_eq!(lib.album_count(), 2, "no invented albums");
    }

    #[test]
    fn scan_reports_progress_and_finishes() {
        let dir = seeded_root();
        let mut phases: Vec<ScanPhase> = Vec::new();
        let mut last_tracks = 0;
        let lib = scan_with_progress(dir.path(), |p| {
            phases.push(p.phase);
            last_tracks = p.tracks;
        });
        assert_eq!(phases.first(), Some(&ScanPhase::Discovering));
        assert_eq!(phases.last(), Some(&ScanPhase::Done));
        assert_eq!(
            phases.iter().filter(|p| **p == ScanPhase::Reading).count(),
            4
        );
        assert_eq!(
            phases.iter().filter(|p| **p == ScanPhase::Artwork).count(),
            2
        );
        assert_eq!(last_tracks, lib.track_count());
    }

    #[test]
    fn scan_of_a_missing_root_is_empty_not_a_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lib = scan(&dir.path().join("does-not-exist"));
        assert!(lib.is_empty());
        assert_eq!(lib.album_count(), 0);
    }

    #[test]
    fn scan_creates_no_cover_cache_when_no_track_has_artwork() {
        let dir = seeded_root();
        let lib = scan(dir.path());
        for key in lib.albums() {
            assert!(!lib.cover_path(key).exists());
        }
    }

    #[test]
    fn cover_cache_shrinks_to_600px_and_writes_a_png() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(800, 400, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 0])
        }));
        let mut png: Vec<u8> = Vec::new();
        src.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encode source");

        let dest: PathBuf = dir.path().join("covers").join("abc.png");
        write_cover_png(&png, &dest).expect("write cover");
        let out = image::open(&dest).expect("decode cover");
        assert_eq!((out.width(), out.height()), (600, 300));

        // Small art is left alone rather than upscaled.
        let small = image::DynamicImage::ImageRgb8(image::RgbImage::new(120, 120));
        let mut small_png: Vec<u8> = Vec::new();
        small
            .write_to(&mut Cursor::new(&mut small_png), image::ImageFormat::Png)
            .expect("encode small");
        let dest2 = dir.path().join("covers").join("small.png");
        write_cover_png(&small_png, &dest2).expect("write small cover");
        let out2 = image::open(&dest2).expect("decode small cover");
        assert_eq!((out2.width(), out2.height()), (120, 120));

        assert!(write_cover_png(b"garbage", &dest2).is_err());
    }

    /// The cache is keyed by (album artist, album), which does not change when the *art*
    /// changes — so freshness has to come from mtimes, and art that goes away has to take
    /// its cache file with it.
    #[test]
    fn cover_cache_follows_the_artwork_it_was_made_from() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let file = root.join("HOME/Odyssey/01 Intro.wav");
        write_wav_with_cover(&file, 300, 400);

        let lib = scan(root);
        let key = AlbumKey::new("HOME", "Odyssey");
        assert!(lib.album(&key).expect("album").has_artwork);
        let dest = lib.cover_path(&key);
        assert_eq!(
            image::open(&dest).expect("cover").width(),
            400,
            "the first scan extracts the cover"
        );

        // The user re-tags the album with different art: the cache file is older than the
        // track it was made from, so the next scan must redo it.
        write_cover_png(&png_bytes(32), &dest).expect("plant a stale cover");
        set_mtime(&dest, SystemTime::UNIX_EPOCH + Duration::from_secs(1_000));
        let lib = scan(root);
        assert_eq!(
            image::open(&dest).expect("cover").width(),
            400,
            "a cover older than its source must be regenerated"
        );
        assert!(lib.album(&key).expect("album").has_artwork);

        // …and a cover that is up to date is left alone (no needless rewriting).
        let before = fs::metadata(&dest)
            .and_then(|m| m.modified())
            .expect("mtime");
        scan(root);
        assert_eq!(
            fs::metadata(&dest)
                .and_then(|m| m.modified())
                .expect("mtime"),
            before,
            "a fresh cover must not be rewritten on every scan"
        );

        // The user strips the artwork: the stale PNG must not keep being painted.
        write_wav(&file, 300);
        let lib = scan(root);
        assert!(!lib.album(&key).expect("album").has_artwork);
        assert!(
            !dest.exists(),
            "the cache file must go when the artwork does"
        );
    }

    /// v1.1: the cover cache lives in the app-data dir, and a scan of a library root that
    /// Phoebus does not own must leave that root exactly as it found it.
    #[test]
    fn a_separate_covers_dir_is_used_and_the_library_root_is_never_written_to() {
        let music = tempfile::tempdir().expect("tempdir");
        let data = tempfile::tempdir().expect("tempdir");
        let covers = data.path().join("cache").join("covers");
        let root = music.path();
        write_wav_with_cover(&root.join("HOME/Odyssey/01 Intro.wav"), 300, 200);

        let before: Vec<_> = fs::read_dir(root)
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();

        let lib = scan_with_covers(root, &covers);
        let key = AlbumKey::new("HOME", "Odyssey");
        assert_eq!(lib.track_count(), 1);
        assert_eq!(lib.covers_dir(), covers);
        assert_eq!(lib.cover_path(&key), covers.join(key.cover_file_name()));
        assert_eq!(
            image::open(lib.cover_path(&key)).expect("cover").width(),
            200,
            "the cover is extracted into the separate cache"
        );

        assert!(
            !root.join(paths::APP_DIR_NAME).exists(),
            "no .phoebus directory may appear inside the library root"
        );
        let after: Vec<_> = fs::read_dir(root)
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(before, after, "the library root is read-only");
    }

    /// The cache must not be able to feed itself back into the library, even when someone
    /// points it at a plainly-named directory inside the music tree.
    #[test]
    fn the_covers_dir_is_pruned_from_the_walk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let covers = root.join("cache-covers");
        write_wav(&root.join("HOME/Odyssey/01 Intro.wav"), 300);
        write_wav(&covers.join("Fake/Album/01 Not Music.wav"), 100);

        let lib = scan_with_covers(root, &covers);
        assert_eq!(lib.track_count(), 1, "the cache directory is not library");
        assert_eq!(lib.album_count(), 1);
    }

    /// Progress reporting is identical on the explicit-covers path.
    #[test]
    fn scan_with_covers_reports_progress() {
        let dir = seeded_root();
        let covers = tempfile::tempdir().expect("tempdir");
        let mut phases: Vec<ScanPhase> = Vec::new();
        let lib = scan_with_covers_progress(dir.path(), covers.path(), |p| phases.push(p.phase));
        assert_eq!(phases.first(), Some(&ScanPhase::Discovering));
        assert_eq!(phases.last(), Some(&ScanPhase::Done));
        assert_eq!(lib.track_count(), 3);
        assert_eq!(lib.covers_dir(), covers.path());
    }

    /// The self-contained entry point keeps the pre-v1.1 in-root layout.
    #[test]
    fn plain_scan_still_caches_covers_under_the_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write_wav_with_cover(&root.join("HOME/Odyssey/01 Intro.wav"), 300, 120);
        let lib = scan(root);
        assert_eq!(lib.covers_dir(), paths::Dirs::inside(root).covers_dir());
        assert!(lib.cover_path(&AlbumKey::new("HOME", "Odyssey")).exists());
    }

    #[test]
    fn year_parsing_is_tolerant() {
        assert_eq!(parse_year("2021"), Some(2021));
        assert_eq!(parse_year("2021-06-01"), Some(2021));
        assert_eq!(parse_year(""), None);
        assert_eq!(parse_year("nope"), None);
        assert_eq!(parse_year("21"), None);
    }

    /// Only a leading `.` hides an entry. A name that is not valid UTF-8 is *not* hidden —
    /// treating it as hidden pruned the whole subtree under it.
    #[cfg(unix)]
    #[test]
    fn only_a_leading_dot_hides_an_entry() {
        use std::os::unix::ffi::OsStrExt;

        assert!(name_is_hidden(OsStr::new(".phoebus")));
        assert!(name_is_hidden(OsStr::new(".hidden.wav")));
        assert!(!name_is_hidden(OsStr::new("HOME")));
        assert!(!name_is_hidden(OsStr::new("")));
        assert!(!name_is_hidden(OsStr::new("a.b")));
        // Latin-1 "Björk": legal on ext4/xfs/btrfs, invalid UTF-8, and not hidden.
        assert!(!name_is_hidden(OsStr::from_bytes(b"Bj\xf6rk")));
        assert!(name_is_hidden(OsStr::from_bytes(b".Bj\xf6rk")));
    }

    /// End-to-end version of the same thing. Self-skips on file systems that reject such
    /// names outright (APFS/HFS+ do); the predicate test above always runs.
    #[cfg(unix)]
    #[test]
    fn scan_keeps_files_whose_names_are_not_utf8_and_can_still_play_them() {
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let album = root.join(OsStr::from_bytes(b"Bj\xf6rk")).join("Post");
        if fs::create_dir_all(&album).is_err() {
            return; // the file system refuses non-UTF-8 names; nothing to exercise
        }
        let file = album.join(OsStr::from_bytes(b"01 Army of M\xe9.wav"));
        write_wav(&file, 300);
        write_wav(&root.join("HOME/Odyssey/01 Intro.wav"), 300);

        let lib = scan(root);
        assert_eq!(lib.track_count(), 2, "the subtree must not be pruned");
        let id = lib
            .tracks_sorted()
            .iter()
            .copied()
            .find(|id| {
                lib.track(*id)
                    .is_some_and(|t| t.rel_path.contains('\u{fffd}'))
            })
            .expect("the lossily-named track is in the library");
        let track = lib.track(id).expect("track");
        assert_eq!(track.album, "Post");
        assert_eq!(
            lib.track_path(id).as_deref(),
            Some(file.as_path()),
            "playback must get the real path, not the lossy rendering"
        );
        assert!(lib.track_path(id).expect("path").exists());
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        assert!(has_audio_extension(Path::new("a/b/C.M4A")));
        assert!(has_audio_extension(Path::new("a/b/c.flac")));
        assert!(!has_audio_extension(Path::new("a/b/c.txt")));
        assert!(!has_audio_extension(Path::new("a/b/c")));
    }
}
