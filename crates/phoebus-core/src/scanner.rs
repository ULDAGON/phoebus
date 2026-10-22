//! Filesystem scan: walk the library root, read tags with `lofty`, build a [`Library`],
//! and keep the cover cache in step with it.
//!
//! A scan can start from the tracks of an earlier one ([`rescan_with_covers_progress`]):
//! files whose size and mtime are unchanged are taken over without being opened, which is
//! what makes the rescan at every launch cheap.
//!
//! The cover cache holds one PNG per distinct picture, named by its [`CoverId`] (a hash of
//! the embedded bytes), so twenty songs with the same art share one file and a compilation
//! whose songs each carry their own gets one file per song. A file is written only when it
//! is missing — its name is its content, so an existing one is never stale — and one that
//! no track refers to any more is deleted at the end of the scan ([`prune_covers`]).
//!
//! Robustness rule: a file that cannot be parsed is skipped with a `log::warn!`. The
//! scanner never panics and never returns an error — a broken library still yields whatever
//! could be read.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::Scope;
use std::time::SystemTime;

use anyhow::{Context, Result};
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::{Accessor, ItemKey};
use lofty::tag::Tag;
use walkdir::{DirEntry, WalkDir};

use crate::model::{CoverId, Library, Track};
use crate::paths;

/// Extensions the scanner picks up (matched case-insensitively).
pub const AUDIO_EXTENSIONS: &[&str] = &["m4a", "mp3", "flac", "ogg", "wav", "aiff", "aac"];

/// Deepest directory nesting the walker descends into.
const MAX_DEPTH: usize = 8;

/// Longest edge of a cached cover PNG, in pixels.
pub const COVER_MAX_EDGE: u32 = 600;

/// Held by a scan from the moment it lists the cover cache until its prune is done.
///
/// A scan trusts the listing it starts from — a cover on disk then is not written again —
/// and its prune deletes what its own library does not use. Two scans of one process
/// overlap only when the app abandons a scan for one of a new root, and the prune of the
/// old one must not delete covers the new one already counted on. So scans take turns on
/// the cache. The wait is short: the abandoned scan is cancelled, and it stops at the next
/// file, writes nothing more and never prunes.
static COVER_CACHE: Mutex<()> = Mutex::new(());

/// How much older than its scan a cache file must be before that scan's prune may delete
/// it. Another process (a second Phoebus on the same data dir) does not take
/// [`COVER_CACHE`], and a file it has just written can carry an mtime from before this scan
/// began: HFS+ keeps whole seconds, a network share has its own clock. A cover that is
/// really unused survives at most until the next scan.
const PRUNE_MARGIN: std::time::Duration = std::time::Duration::from_secs(60);

/// Which stage of the scan a [`ScanProgress`] belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScanPhase {
    /// Walking directories, before any file has been read.
    Discovering,
    /// Reading tags, one audio file at a time.
    Reading,
    /// Extracting the covers that the cache lost although their tracks did not change.
    /// New and changed files have their cover written while they are read.
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
    /// What is being worked on (relative path, or song title during the artwork phase).
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

/// [`scan_with_covers`] with a progress callback: a full scan, reading every file.
pub fn scan_with_covers_progress<F: FnMut(ScanProgress)>(
    root: &Path,
    covers_dir: &Path,
    progress: F,
) -> Library {
    rescan_with_covers_progress(root, covers_dir, Vec::new(), progress)
}

/// [`scan_with_covers_progress`] that reuses what an earlier scan of the same `root` already
/// read — the one function every other scan delegates to.
///
/// `known` is that scan's tracks ([`Library::tracks`], or a [library
/// snapshot](crate::snapshot)'s). The walk itself is unchanged, so new, moved and deleted
/// files are found exactly as before; what changes is the cost per file. A file whose size
/// and mtime both match its known track is taken over as it is, for the price of the `stat`
/// the walk already pays. Only new and changed files are opened with `lofty`, which is where
/// a full scan spends nearly all of its time — it is I/O, not CPU, and on a large library it
/// is seconds. An empty `known` is a full scan.
///
/// The walk skips every dot-entry (so `<root>/.phoebus` is invisible whatever `covers_dir`
/// is) and additionally prunes `covers_dir` itself, so pointing the cache at a plainly-named
/// directory inside the library cannot feed the cache back into the library.
pub fn rescan_with_covers_progress<F: FnMut(ScanProgress)>(
    root: &Path,
    covers_dir: &Path,
    known: impl IntoIterator<Item = Track>,
    progress: F,
) -> Library {
    rescan_cancellable(root, covers_dir, known, || false, progress)
}

/// [`rescan_with_covers_progress`] that can be called off.
///
/// `cancelled` is polled between files, between restored covers and before the prune. Once
/// it says yes the scan stops reading and, above all, stops touching `covers_dir` — the
/// cover cache is shared by every scan of the run, so a scan the app has given up on (its
/// library root changed) must not keep deleting and rewriting covers the new root's
/// library is using. The covers still queued for the writers are dropped, and a cancelled
/// scan never prunes. What comes back is then an incomplete library that the caller must
/// throw away; at most the covers the writers already hold — one each — are still
/// finished, atomically.
pub fn rescan_cancellable<F: FnMut(ScanProgress)>(
    root: &Path,
    covers_dir: &Path,
    known: impl IntoIterator<Item = Track>,
    cancelled: impl Fn() -> bool,
    mut progress: F,
) -> Library {
    // This scan's turn on the cover cache, for as long as it reads or changes it.
    let _cache = COVER_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    // Taken before anything is written: the prune at the end must not delete what a scan
    // running beside this one writes meanwhile (see `prune_covers`).
    let started = SystemTime::now();
    progress(ScanProgress {
        phase: ScanPhase::Discovering,
        done: 0,
        total: None,
        tracks: 0,
        current: None,
    });

    // Keyed by the real path, not the id: two lossily-named files can share an id, never a
    // path.
    let mut known: HashMap<PathBuf, Track> =
        known.into_iter().map(|t| (t.abs_path(root), t)).collect();
    let mut tracks: Vec<Track> = Vec::new();
    let mut files_seen = 0usize;
    let mut reused = 0usize;
    // The walk could not even open the root: a missing, unmounted or denied library.
    let mut root_unreadable = false;

    let walker = WalkDir::new(root)
        .max_depth(MAX_DEPTH)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !is_hidden(e) && e.path() != covers_dir);

    // The cover writers live exactly as long as the scan: leaving the scope waits for the
    // last PNG, so the library is never handed out before its covers are on disk.
    let library = std::thread::scope(|scope| {
        let mut covers = CoverWriter::start(scope, covers_dir);
        for entry in walker {
            if cancelled() {
                // From here on nothing may reach the shared cache: the writers drop
                // what is still queued.
                covers.cancel();
                break;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    root_unreadable |= e.depth() == 0;
                    log::warn!("scan: skipping unreadable entry: {e}");
                    continue;
                }
            };
            if !entry.file_type().is_file() || !has_audio_extension(entry.path()) {
                continue;
            }
            files_seen += 1;
            let stamp = FileStamp::of(&entry);
            let track = match known.remove(entry.path()) {
                Some(track) if stamp.matches(&track) => {
                    reused += 1;
                    Some(track)
                }
                _ => read_track(root, entry.path(), stamp, &mut covers),
            };
            let rel = track.as_ref().map(|t| t.rel_path.clone());
            tracks.extend(track);
            progress(ScanProgress {
                phase: ScanPhase::Reading,
                done: files_seen,
                total: None,
                tracks: tracks.len(),
                current: rel,
            });
        }
        log::info!(
            "scan: {files_seen} audio files, {reused} unchanged, {} read",
            files_seen - reused
        );

        let library =
            Library::build_with_covers(root.to_path_buf(), covers_dir.to_path_buf(), tracks);
        restore_covers(root, &library, &mut covers, &cancelled, &mut progress);
        if cancelled() {
            covers.cancel();
        }
        covers.finish();
        library
    });
    // A cancelled scan's library is incomplete: pruning by it would delete covers that the
    // library the app keeps (or the new root's) still uses. So is the library of a root
    // that could not be read or held nothing — an unmounted drive, an offline NAS, a
    // denied folder: pruning by it would empty the whole cache, and the next scan of the
    // root, back again, would have to extract every cover anew.
    if root_unreadable || library.is_empty() {
        log::info!(
            "scan: {} gave no tracks; the cover cache is left as it is",
            root.display()
        );
    } else if !cancelled() {
        prune_covers(covers_dir, &library.cover_ids(), started, &cancelled);
    }

    progress(ScanProgress {
        phase: ScanPhase::Done,
        done: library.track_count(),
        total: Some(library.track_count()),
        tracks: library.track_count(),
        current: None,
    });
    library
}

/// What the walk learns about a file from its `stat`: enough to tell whether the track an
/// earlier scan read from it is still current.
#[derive(Clone, Copy, Debug)]
struct FileStamp {
    /// `None` when the file could not be stat'ed: such a file is always read, and its
    /// track can never be reused.
    mtime: Option<SystemTime>,
    size: u64,
    /// Some permission bit lets someone read the file. Without one (`chmod 000`) the file
    /// cannot have stayed readable: neither size nor mtime moves on a `chmod`, so this is
    /// the only trace it leaves in the `stat` the walk already has. Always true off Unix.
    readable: bool,
}

impl FileStamp {
    /// The stamp of a walked entry. The walk does not follow links, so this is the file's
    /// own metadata, and on most platforms it is already cached in the entry.
    fn of(entry: &DirEntry) -> FileStamp {
        match entry.metadata() {
            Ok(m) => FileStamp {
                mtime: m.modified().ok(),
                size: m.len(),
                readable: has_read_bit(&m),
            },
            Err(e) => {
                log::warn!("scan: no metadata for {} ({e})", entry.path().display());
                FileStamp {
                    mtime: None,
                    size: 0,
                    readable: false,
                }
            }
        }
    }

    /// Same size and same mtime, to the nanosecond: the file has not been written since
    /// `track` was read from it. And still readable, so a file that became unreadable is
    /// read again — and skipped, as a full scan would skip it — instead of staying listed
    /// with a track nobody can play.
    fn matches(self, track: &Track) -> bool {
        self.readable && self.mtime == Some(track.added_at) && self.size == track.size
    }
}

/// Whether any of the owner, group or other read bits is set. A cheap stand-in for "can be
/// opened": it needs no `open`, only the metadata in hand.
#[cfg(unix)]
fn has_read_bit(m: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    m.permissions().mode() & 0o444 != 0
}

#[cfg(not(unix))]
fn has_read_bit(_: &fs::Metadata) -> bool {
    true
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
///
/// The first embedded picture is hashed into [`Track::cover`] here, where lofty has already
/// read it into memory, and handed to `covers` — which copies it only if the cache does not
/// have that picture yet. So a cover costs no second open of the file, and twenty songs
/// with the same art cost one PNG.
fn read_track(
    root: &Path,
    path: &Path,
    stamp: FileStamp,
    covers: &mut CoverWriter<'_>,
) -> Option<Track> {
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
    track.added_at = stamp.mtime.unwrap_or_else(SystemTime::now);
    track.size = stamp.size;

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
        if let Some(data) = cover_picture(tag, path) {
            let id = CoverId::of_picture(data);
            covers.offer(id, data, path);
            track.cover = Some(id);
        }
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

/// How many cover PNGs may wait for a writer at once — each one holds an embedded picture
/// (typically a few hundred KB, rarely a few MB), so this is what bounds the memory a first
/// scan of a few thousand songs spends on art.
const COVER_QUEUE_PER_WRITER: usize = 2;

/// Most threads that decode, resize and encode covers during a scan. Writing a PNG is CPU
/// (tens of ms each), reading tags is I/O; a first scan of a compilation-heavy library
/// writes hundreds of covers, and doing that on the walking thread made the walk wait for
/// the encoder. Capped well below the core count: the UI and the audio thread keep theirs.
const MAX_COVER_WRITERS: usize = 4;

/// One cover PNG to write.
struct CoverJob {
    bytes: Vec<u8>,
    dest: PathBuf,
    /// The file the picture came from, for the warning when it cannot be decoded.
    source: PathBuf,
}

/// The write side of the cover cache during one scan: decides which pictures need a PNG and
/// hands them to a small pool of writer threads over a bounded queue.
///
/// Each distinct picture is written at most once per scan and only when the cache does not
/// already have it. Without any writer thread (the OS refused to spawn one) it writes on
/// the calling thread instead: slower, never wrong.
struct CoverWriter<'a> {
    dir: &'a Path,
    /// Ids with a PNG in the cache — on disk when the scan started, or queued since.
    have: HashSet<CoverId>,
    jobs: Option<SyncSender<CoverJob>>,
    /// Set when the scan is cancelled: the writers drop every job they have not started.
    /// A flag rather than the scan's own `cancelled`, which need not be callable from
    /// another thread.
    stop: Arc<AtomicBool>,
    /// PNGs queued (or written inline) by this scan.
    queued: usize,
}

impl<'a> CoverWriter<'a> {
    /// Read what the cache holds and start the writers inside `scope`.
    fn start<'scope>(scope: &'scope Scope<'scope, '_>, dir: &'a Path) -> Self {
        let writers = std::thread::available_parallelism()
            .map_or(1, |n| n.get().saturating_sub(2))
            .clamp(1, MAX_COVER_WRITERS);
        let (tx, rx) = std::sync::mpsc::sync_channel::<CoverJob>(writers * COVER_QUEUE_PER_WRITER);
        let rx = Arc::new(Mutex::new(rx));
        let stop = Arc::new(AtomicBool::new(false));
        let mut spawned = 0;
        for i in 0..writers {
            let rx = Arc::clone(&rx);
            let stop = Arc::clone(&stop);
            match std::thread::Builder::new()
                .name(format!("phoebus-covers-{i}"))
                .spawn_scoped(scope, move || cover_worker(&rx, &stop))
            {
                Ok(_) => spawned += 1,
                Err(e) => log::warn!("scan: no cover writer thread ({e})"),
            }
        }
        CoverWriter {
            dir,
            have: cached_covers(dir),
            jobs: (spawned > 0).then_some(tx),
            stop,
            queued: 0,
        }
    }

    /// Make sure the cache will hold `id`, whose picture is `bytes` (from `source`).
    /// Copies the bytes only when it has to write them.
    fn offer(&mut self, id: CoverId, bytes: &[u8], source: &Path) {
        if self.stop.load(Ordering::Acquire) || !self.have.insert(id) {
            return;
        }
        self.queued += 1;
        let job = CoverJob {
            bytes: bytes.to_vec(),
            dest: self.dir.join(id.file_name()),
            source: source.to_path_buf(),
        };
        let job = match &self.jobs {
            // Blocks while the queue is full: that is the memory bound.
            Some(jobs) => match jobs.send(job) {
                Ok(()) => return,
                Err(std::sync::mpsc::SendError(job)) => job, // every writer is gone
            },
            None => job,
        };
        write_job(&job);
    }

    /// Whether the cache holds (or will hold) `id`.
    fn has(&self, id: CoverId) -> bool {
        self.have.contains(&id)
    }

    /// The scan was cancelled: write nothing more. Jobs a writer has already started are
    /// finished (each PNG lands atomically or not at all); the rest are dropped.
    fn cancel(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Close the queue, so the writers stop once it is empty. The scope then joins them.
    fn finish(self) {
        if self.queued > 0 {
            log::info!("scan: {} new cover(s) for the cache", self.queued);
        }
    }
}

/// One writer thread: take jobs until the queue is closed and empty, writing them unless
/// the scan was cancelled meanwhile.
fn cover_worker(jobs: &Mutex<Receiver<CoverJob>>, stop: &AtomicBool) {
    loop {
        // The lock is held only while waiting for a job, never while encoding one.
        let job = match jobs.lock() {
            Ok(rx) => rx.recv(),
            Err(_) => return,
        };
        let Ok(job) = job else {
            return; // the scan is done
        };
        if !stop.load(Ordering::Acquire) {
            write_job(&job);
        }
    }
}

/// Write one cover. A picture the image decoder chokes on — or even panics on, which a
/// hostile file could make it do — costs that one cover, never the scan.
fn write_job(job: &CoverJob) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        write_cover_png(&job.bytes, &job.dest)
    }));
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => log::warn!("scan: cover from {} failed: {e:#}", job.source.display()),
        Err(_) => log::warn!(
            "scan: cover from {} crashed the decoder; skipped",
            job.source.display()
        ),
    }
}

/// The covers the cache holds right now: every non-empty file in `dir` with a cache name.
/// A missing or unreadable directory holds none.
///
/// An empty file is not a cover — the remains of a full disk, or of a copy cut short — and
/// counting it as one would keep it forever, since a cover on disk is never written again.
/// Left out, it is overwritten by this scan. (A non-empty file that does not decode is
/// caught where it is read: the app's cover loader deletes it, and the next scan restores
/// it.) The size is in the `stat` the listing makes anyway; the bytes are not read.
fn cached_covers(dir: &Path) -> HashSet<CoverId> {
    let Ok(entries) = fs::read_dir(dir) else {
        return HashSet::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.metadata().is_ok_and(|m| m.is_file() && m.len() > 0))
        .filter_map(|e| e.file_name().to_str().and_then(CoverId::from_file_name))
        .collect()
}

/// Whether the cached cover at `path` can be handed to something that must not get a bad
/// image: it exists and starts like a PNG, which every file the cache writes does. One
/// short read, no decode. The OS Now Playing art is the case: a `cover_url` macOS cannot
/// load aborts the process.
pub fn cover_file_is_sound(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 8];
    fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && head == *b"\x89PNG\r\n\x1a\n"
}

/// Write the covers the cache lacks although no file changed: the tracks were reused from an
/// earlier scan (so their pictures were never in memory this time), but the PNG is gone —
/// a cleared cache, or a library snapshot that outlived its cache. Each such picture is
/// read once more, from the first song that carries it and still holds it.
///
/// "Still holds it" is checked: the picture read must hash to the id. A file re-tagged
/// without its size or mtime moving, or one that cannot be read, is passed over for the
/// next song with the same cover, so one bad file does not cost every song that shares
/// its art. Only when no song yields the picture does the cover stay missing (the songs
/// paint the placeholder) until a `RESCAN` reads them again.
fn restore_covers<F: FnMut(ScanProgress)>(
    root: &Path,
    library: &Library,
    covers: &mut CoverWriter<'_>,
    cancelled: &impl Fn() -> bool,
    progress: &mut F,
) {
    if cancelled() {
        return;
    }
    // Each missing cover with every song that carries it, both in Songs order.
    let mut missing: Vec<(CoverId, Vec<&Track>)> = Vec::new();
    let mut index: HashMap<CoverId, usize> = HashMap::new();
    for id in library.tracks_sorted() {
        if let Some(track) = library.track(*id)
            && let Some(cover) = track.cover
            && !covers.has(cover)
        {
            let i = *index.entry(cover).or_insert_with(|| {
                missing.push((cover, Vec::new()));
                missing.len() - 1
            });
            missing[i].1.push(track);
        }
    }
    let total = missing.len();
    for (i, (cover, carriers)) in missing.into_iter().enumerate() {
        if cancelled() {
            return;
        }
        progress(ScanProgress {
            phase: ScanPhase::Artwork,
            done: i + 1,
            total: Some(total),
            tracks: library.track_count(),
            current: carriers.first().map(|t| t.title.clone()),
        });
        let restored = carriers.iter().any(|track| {
            let source = track.abs_path(root);
            match first_picture(&source) {
                Some(data) if CoverId::of_picture(&data) == cover => {
                    covers.offer(cover, &data, &source);
                    true
                }
                Some(_) => {
                    log::warn!(
                        "scan: the picture in {} changed without its mtime; RESCAN reads it again",
                        source.display()
                    );
                    false
                }
                None => {
                    log::warn!("scan: no readable picture in {}", source.display());
                    false
                }
            }
        });
        if !restored {
            log::warn!(
                "scan: cover {cover} is in none of its {} song(s)",
                carriers.len()
            );
        }
    }
}

/// Delete every cached cover that no track of the library refers to any more — including
/// the per-album files of earlier versions, which share the naming pattern.
///
/// Only files whose name is exactly a cache name ([`CoverId::from_file_name`]) are ever
/// considered, so nothing else that sits in the directory is touched. Only files older
/// than `started`, the moment this scan began, by [`PRUNE_MARGIN`]: a file written since
/// may be another process's (scans of this one take turns, [`COVER_CACHE`]). And it stops
/// the moment `cancelled` says so, between two files: a scan abandoned while it prunes
/// deletes nothing more.
fn prune_covers(
    dir: &Path,
    keep: &HashSet<CoverId>,
    started: SystemTime,
    cancelled: &impl Fn() -> bool,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let cutoff = started
        .checked_sub(PRUNE_MARGIN)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut removed = 0usize;
    for entry in entries.filter_map(Result::ok) {
        if cancelled() {
            break;
        }
        let Some(id) = entry.file_name().to_str().and_then(CoverId::from_file_name) else {
            continue;
        };
        if keep.contains(&id) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() || meta.modified().map_or(true, |at| at >= cutoff) {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,
            Err(e) => log::warn!(
                "scan: unused cover {} stayed behind: {e}",
                entry.path().display()
            ),
        }
    }
    if removed > 0 {
        log::info!("scan: removed {removed} unused cover(s) from the cache");
    }
}

/// The picture of a tag that is the song's cover: the first one the cover cache can
/// decode, whatever its `PictureType` (real files tag their cover as `Other`).
///
/// "Can decode" is PNG or JPEG, told by the leading bytes: those are the only decoders the
/// app builds `image` with. A WebP or GIF cover, or an ID3 picture that is a `-->` link
/// rather than an image, would get an id but never a PNG — a cover that counts as one of
/// an album's covers (a mosaic tile of `♪`) and that the restore pass reads again at every
/// launch. Such a picture is passed over for the next one; with none left the song has no
/// cover, and says why once.
fn cover_picture<'t>(tag: &'t Tag, path: &Path) -> Option<&'t [u8]> {
    let pictures = tag.pictures();
    let found = pictures
        .iter()
        .map(|p| p.data())
        .find(|data| is_png_or_jpeg(data));
    if found.is_none() && !pictures.is_empty() {
        log::warn!(
            "scan: {} carries art the cover cache cannot read (only PNG and JPEG); no cover",
            path.display()
        );
    }
    found
}

/// Whether `data` starts like a PNG or a JPEG file.
fn is_png_or_jpeg(data: &[u8]) -> bool {
    data.starts_with(b"\x89PNG\r\n\x1a\n") || data.starts_with(&[0xFF, 0xD8, 0xFF])
}

/// The cover picture of a file ([`cover_picture`]), read from disk once more.
fn first_picture(path: &Path) -> Option<Vec<u8>> {
    let tagged = lofty::read_from_path(path).ok()?;
    let tag: &Tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    cover_picture(tag, path).map(<[u8]>::to_vec)
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

    /// A WAV whose ID3v2 tag carries `pictures`, in this order.
    fn write_wav_with_pictures(path: &Path, pictures: Vec<Vec<u8>>) {
        use lofty::config::WriteOptions;
        use lofty::picture::{Picture, PictureType};
        use lofty::tag::{Tag, TagExt, TagType};

        write_wav(path, 200);
        let mut tag = Tag::new(TagType::Id3v2);
        for data in pictures {
            tag.push_picture(
                Picture::unchecked(data)
                    .pic_type(PictureType::Other)
                    .build(),
            );
        }
        tag.save_to_path(path, WriteOptions::default())
            .expect("embed pictures");
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
        assert_eq!(t.cover, None);
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
        assert!(!odyssey.has_artwork());
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
            0,
            "covers of files that were read are written while reading"
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
        assert!(lib.cover_ids().is_empty());
        assert!(
            cache_files(&lib.covers_dir())
                .iter()
                .all(|name| CoverId::from_file_name(name).is_none()),
            "only the seeded decoy is in there"
        );
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

    /// The single cover of the only album of `lib`.
    fn only_cover(lib: &Library) -> CoverId {
        let key = lib.albums().first().expect("an album");
        let album = lib.album(key).expect("album");
        assert_eq!(album.covers.len(), 1, "{:?}", album.covers);
        album.covers[0]
    }

    /// Make every file in `dir` look long written — what the time between two real scans
    /// does, so the prune's margin for fresh files does not apply.
    fn age_cache(dir: &Path) {
        for name in cache_files(dir) {
            set_mtime(
                &dir.join(name),
                SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000),
            );
        }
    }

    /// The cache files in `dir`, sorted.
    fn cache_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// The cache is named by content, so it follows the art by itself: new art is a new
    /// file, the old one goes, an unchanged one is never rewritten, and art that goes away
    /// takes its file with it.
    #[test]
    fn cover_cache_follows_the_artwork_it_was_made_from() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let file = root.join("HOME/Odyssey/01 Intro.wav");
        write_wav_with_cover(&file, 300, 400);

        let lib = scan(root);
        let first = only_cover(&lib);
        let dest = lib.cover_path(first);
        assert_eq!(
            image::open(&dest).expect("cover").width(),
            400,
            "the first scan extracts the cover"
        );

        // An unchanged cover is left alone (no needless rewriting).
        let before = fs::metadata(&dest)
            .and_then(|m| m.modified())
            .expect("mtime");
        scan(root);
        assert_eq!(
            fs::metadata(&dest)
                .and_then(|m| m.modified())
                .expect("mtime"),
            before,
            "an existing cover must not be rewritten on every scan"
        );

        // The user re-tags the album with different art, some time later: a new id, a
        // new file, and the old file is gone.
        age_cache(&lib.covers_dir());
        write_wav_with_cover(&file, 300, 200);
        let lib = scan(root);
        let second = only_cover(&lib);
        assert_ne!(second, first);
        assert_eq!(
            image::open(lib.cover_path(second)).expect("cover").width(),
            200
        );
        assert!(!dest.exists(), "the old art's file must go");

        // The user strips the artwork: the PNG must not keep being painted.
        age_cache(&lib.covers_dir());
        write_wav(&file, 300);
        let lib = scan(root);
        assert!(
            !lib.album(&AlbumKey::new("HOME", "Odyssey"))
                .expect("album")
                .has_artwork()
        );
        assert!(
            cache_files(&lib.covers_dir()).is_empty(),
            "the cache file must go when the artwork does"
        );
    }

    /// Twenty songs with one picture are one PNG; songs with their own pictures are one PNG
    /// each, and each song knows its own.
    #[test]
    fn the_cover_cache_holds_each_distinct_picture_once() {
        let music = tempfile::tempdir().expect("tempdir");
        let covers = tempfile::tempdir().expect("tempdir");
        let root = music.path();
        for n in 1..=3 {
            write_wav_with_cover(&root.join(format!("HOME/Odyssey/0{n} Same.wav")), 200, 64);
        }
        write_wav_with_cover(&root.join("Various/Mixed/01 One.wav"), 200, 32);
        write_wav_with_cover(&root.join("Various/Mixed/02 Two.wav"), 200, 48);
        write_wav_with_cover(&root.join("Various/Mixed/03 Again.wav"), 200, 32);
        write_wav(&root.join("Various/Mixed/04 Bare.wav"), 200);

        let lib = scan_with_covers(root, covers.path());
        let odyssey = lib.album(&AlbumKey::new("HOME", "Odyssey")).expect("album");
        assert_eq!(odyssey.covers.len(), 1, "one picture, one cover");
        let mixed = lib
            .album(&AlbumKey::new("Various", "Mixed"))
            .expect("album");
        assert_eq!(mixed.covers.len(), 2, "two distinct pictures");
        let cover_of = |rel: &str| lib.track(TrackId::for_rel_path(rel)).and_then(|t| t.cover);
        assert_eq!(cover_of("Various/Mixed/01 One.wav"), Some(mixed.covers[0]));
        assert_eq!(cover_of("Various/Mixed/02 Two.wav"), Some(mixed.covers[1]));
        assert_eq!(
            cover_of("Various/Mixed/03 Again.wav"),
            Some(mixed.covers[0])
        );
        assert_eq!(cover_of("Various/Mixed/04 Bare.wav"), None);

        assert_eq!(
            cache_files(covers.path()).len(),
            3,
            "five songs with art, three pictures, three PNGs"
        );
        for id in lib.cover_ids() {
            let width = image::open(lib.cover_path(id)).expect("cover").width();
            assert!([64, 32, 48].contains(&width), "{id}: {width}");
        }
    }

    /// The prune deletes unused cache files — the per-album ones of earlier versions too —
    /// and nothing that is not a cache file, and nothing written after the scan began.
    #[test]
    fn the_prune_only_touches_unused_cache_files() {
        let music = tempfile::tempdir().expect("tempdir");
        let covers = tempfile::tempdir().expect("tempdir");
        let root = music.path();
        write_wav_with_cover(&root.join("HOME/Odyssey/01 Intro.wav"), 200, 40);
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let foreign = [
            "notes.txt",
            "cover.png",
            // Not the legacy name in capitals: APFS would make that the same file.
            "ABCDEF0123456789.png",
            "176f932674e9821f.jpg",
            "176f932674e9821f.png.tmp",
        ];
        for name in foreign {
            fs::write(covers.path().join(name), b"not ours").expect("write");
            set_mtime(&covers.path().join(name), old);
        }
        // What an album-keyed cache of an earlier version left behind.
        let legacy = covers.path().join("176f932674e9821f.png");
        write_cover_png(&png_bytes(8), &legacy).expect("legacy cover");
        set_mtime(&legacy, old);
        fs::create_dir(covers.path().join("0123456789abcdef.png")).expect("a dir with our name");

        let lib = scan_with_covers(root, covers.path());
        let cover = only_cover(&lib);
        assert!(!legacy.exists(), "an unused cache file is deleted");
        let mut expected: Vec<String> = foreign.iter().map(|n| (*n).to_string()).collect();
        expected.push(cover.file_name());
        expected.push("0123456789abcdef.png".to_string());
        expected.sort();
        assert_eq!(
            cache_files(covers.path()),
            expected,
            "nothing else is touched"
        );

        // A file written after the scan began may be another scan's: it stays — and so
        // does one that only looks a little older, as a coarse or skewed mtime can.
        let fresh = covers.path().join("00000000000000ab.png");
        write_cover_png(&png_bytes(8), &fresh).expect("fresh cover");
        let keep = lib.cover_ids();
        let now = SystemTime::now();
        prune_covers(covers.path(), &keep, now - Duration::from_secs(60), &|| {
            false
        });
        assert!(fresh.exists(), "younger than the scan");
        set_mtime(&fresh, now - Duration::from_secs(5));
        prune_covers(covers.path(), &keep, now, &|| false);
        assert!(
            fresh.exists(),
            "within the margin: perhaps another process's"
        );
        // A cancelled prune deletes nothing.
        let later = now + PRUNE_MARGIN + Duration::from_secs(60);
        prune_covers(covers.path(), &keep, later, &|| true);
        assert!(
            fresh.exists(),
            "a cancelled prune stops before the first file"
        );
        prune_covers(covers.path(), &keep, later, &|| false);
        assert!(
            !fresh.exists(),
            "older than the scan by the margin, and unused"
        );
        assert!(lib.cover_path(cover).exists(), "a used cover always stays");
    }

    /// An empty file under a cover's name is not that cover: the next scan writes it, and
    /// only a file that starts like a PNG counts as sound.
    #[test]
    fn an_empty_cache_file_is_written_again() {
        let music = tempfile::tempdir().expect("tempdir");
        let covers = tempfile::tempdir().expect("tempdir");
        let root = music.path();
        write_wav_with_cover(&root.join("HOME/Odyssey/01 Intro.wav"), 200, 40);
        let lib = scan_with_covers(root, covers.path());
        let path = lib.cover_path(only_cover(&lib));
        assert!(cover_file_is_sound(&path));

        fs::write(&path, b"").expect("empty it");
        assert!(!cover_file_is_sound(&path));
        rescan_with_covers_progress(root, covers.path(), lib.tracks().cloned(), |_| {});
        assert!(
            cover_file_is_sound(&path),
            "restored although the track was reused"
        );

        fs::write(&path, b"not a png at all").expect("garble it");
        assert!(!cover_file_is_sound(&path));
        assert!(!cover_file_is_sound(&covers.path().join("missing.png")));
    }

    /// The first song carrying a lost cover may no longer yield it (here: damaged in
    /// place, size and mtime kept, so the rescan reuses its track). The next one does.
    #[test]
    fn a_lost_cover_is_restored_from_the_next_song_that_carries_it() {
        let music = tempfile::tempdir().expect("tempdir");
        let covers = tempfile::tempdir().expect("tempdir");
        let root = music.path();
        let first = root.join("HOME/Odyssey/01 a.wav");
        write_wav_with_cover(&first, 200, 40);
        write_wav_with_cover(&root.join("HOME/Odyssey/02 b.wav"), 200, 40);
        let lib = scan_with_covers(root, covers.path());
        let cover = only_cover(&lib);
        fs::remove_file(lib.cover_path(cover)).expect("lose the cover");

        let stamp = fs::metadata(&first).expect("stat");
        let mtime = stamp.modified().expect("mtime");
        fs::write(&first, vec![0u8; stamp.len() as usize]).expect("damage in place");
        set_mtime(&first, mtime);

        let again = rescan_with_covers_progress(root, covers.path(), lib.tracks().cloned(), |_| {});
        assert_eq!(again.track_count(), 2, "both tracks reused, unread");
        assert!(
            again.cover_path(cover).exists(),
            "restored from the second song"
        );
    }

    /// Only a picture the cache can decode becomes a cover: a GIF first in the tag is passed
    /// over for the PNG behind it, and a song with only undecodable art has no cover — so
    /// it neither turns its album into a mosaic of `♪` tiles nor fails the cache.
    #[test]
    fn only_png_or_jpeg_art_becomes_a_cover() {
        let music = tempfile::tempdir().expect("tempdir");
        let covers = tempfile::tempdir().expect("tempdir");
        let root = music.path();
        let gif = b"GIF89a\x01\x00\x01\x00 not decodable here".to_vec();
        let link = b"-->https://example.com/cover.jpg".to_vec();
        let png = png_bytes(24);
        write_wav_with_pictures(&root.join("HOME/Odyssey/01 a.wav"), vec![png.clone()]);
        write_wav_with_pictures(
            &root.join("HOME/Odyssey/02 b.wav"),
            vec![gif.clone(), png.clone()],
        );
        write_wav_with_pictures(&root.join("HOME/Odyssey/03 c.wav"), vec![gif, link]);

        let lib = scan_with_covers(root, covers.path());
        let cover_of = |rel: &str| lib.track(TrackId::for_rel_path(rel)).and_then(|t| t.cover);
        let png_id = CoverId::of_picture(&png);
        assert_eq!(cover_of("HOME/Odyssey/01 a.wav"), Some(png_id));
        assert_eq!(
            cover_of("HOME/Odyssey/02 b.wav"),
            Some(png_id),
            "the GIF is passed over for the PNG"
        );
        assert_eq!(cover_of("HOME/Odyssey/03 c.wav"), None, "nothing decodable");
        let album = lib.album(&AlbumKey::new("HOME", "Odyssey")).expect("album");
        assert_eq!(album.mosaic(), &[png_id], "one cover, no ♪ tile");
        assert_eq!(cache_files(covers.path()), vec![png_id.file_name()]);
        assert!(is_png_or_jpeg(&[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(!is_png_or_jpeg(b"RIFF\0\0\0\0WEBPVP8 "));
    }

    /// Scans take turns on the cover cache: one that starts while another holds it waits,
    /// so the other's prune can never delete covers it has already counted on.
    #[test]
    fn scans_take_turns_on_the_cover_cache() {
        let music = tempfile::tempdir().expect("tempdir");
        let covers = tempfile::tempdir().expect("tempdir");
        write_wav_with_cover(&music.path().join("HOME/Odyssey/01 Intro.wav"), 200, 40);
        let turn = COVER_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        std::thread::scope(|s| {
            let other = s.spawn(|| scan_with_covers(music.path(), covers.path()));
            std::thread::sleep(Duration::from_millis(200));
            assert!(!other.is_finished(), "the second scan waits for its turn");
            assert!(
                cache_files(covers.path()).is_empty(),
                "…before it touches the cache"
            );
            drop(turn);
            let lib = other.join().expect("scan");
            assert_eq!(
                cache_files(covers.path()),
                vec![only_cover(&lib).file_name()]
            );
        });
    }

    /// An unmounted drive scans as an unreadable root, an emptied mount point as a root
    /// without music. Neither may cost the cache a single cover.
    #[test]
    fn a_scan_that_finds_nothing_leaves_the_cache_alone() {
        let music = tempfile::tempdir().expect("tempdir");
        let covers = tempfile::tempdir().expect("tempdir");
        let root = music.path().join("Volume");
        write_wav_with_cover(&root.join("HOME/Odyssey/01 Intro.wav"), 200, 40);
        let lib = scan_with_covers(&root, covers.path());
        let cover = only_cover(&lib);
        age_cache(covers.path());

        // The volume is gone: the root itself cannot be read.
        fs::rename(&root, music.path().join("Elsewhere")).expect("unmount");
        assert!(scan_with_covers(&root, covers.path()).is_empty());
        assert!(
            lib.cover_path(cover).exists(),
            "an unreadable root prunes nothing"
        );

        // The mount point is back, but empty.
        fs::create_dir(&root).expect("empty mount point");
        assert!(scan_with_covers(&root, covers.path()).is_empty());
        assert!(
            lib.cover_path(cover).exists(),
            "an empty library prunes nothing"
        );
    }

    /// A rescan reuses tracks without opening their files, so their pictures are not in
    /// memory — but a cover the cache lost is still restored, from the first song that
    /// carries it.
    #[test]
    fn a_rescan_restores_covers_the_cache_lost() {
        let music = tempfile::tempdir().expect("tempdir");
        let covers = tempfile::tempdir().expect("tempdir");
        let root = music.path();
        write_wav_with_cover(&root.join("Various/Mixed/01 One.wav"), 200, 32);
        write_wav_with_cover(&root.join("Various/Mixed/02 Two.wav"), 200, 48);
        write_wav_with_cover(&root.join("Various/Mixed/03 Again.wav"), 200, 32);
        let first = scan_with_covers(root, covers.path());
        assert_eq!(cache_files(covers.path()).len(), 2);

        for name in cache_files(covers.path()) {
            fs::remove_file(covers.path().join(name)).expect("clear the cache");
        }
        let mut phases: Vec<ScanPhase> = Vec::new();
        let again = rescan_with_covers_progress(root, covers.path(), marked(&first), |p| {
            phases.push(p.phase);
        });
        assert_eq!(
            genre_of(&again, "Various/Mixed/01 One.wav").as_deref(),
            Some("FROM THE LAST SCAN"),
            "the tracks were reused, not read"
        );
        assert_eq!(
            phases.iter().filter(|p| **p == ScanPhase::Artwork).count(),
            2,
            "one restore per distinct cover"
        );
        for id in again.cover_ids() {
            assert!(again.cover_path(id).exists(), "{id} is back");
        }
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
        assert_eq!(lib.track_count(), 1);
        assert_eq!(lib.covers_dir(), covers);
        let cover = only_cover(&lib);
        assert_eq!(lib.cover_path(cover), covers.join(cover.file_name()));
        assert_eq!(
            image::open(lib.cover_path(cover)).expect("cover").width(),
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
        assert!(lib.cover_path(only_cover(&lib)).exists());
    }

    /// Every track a scan read, with one field changed that a real read would never
    /// produce — so a rescan that hands it back provably did not open the file.
    fn marked(lib: &Library) -> Vec<Track> {
        lib.tracks()
            .cloned()
            .map(|mut t| {
                t.genre = Some("FROM THE LAST SCAN".to_string());
                t
            })
            .collect()
    }

    fn genre_of(lib: &Library, rel: &str) -> Option<String> {
        lib.track(TrackId::for_rel_path(rel))
            .and_then(|t| t.genre.clone())
    }

    #[test]
    fn a_rescan_reuses_unchanged_files_and_rereads_changed_ones() {
        let dir = seeded_root();
        let root = dir.path();
        let first = scan(root);
        assert!(
            first.tracks().all(|t| t.size > 0),
            "the scan records every file's size"
        );

        // One file is rewritten (longer, so the size moves too), one is deleted, one is new.
        write_wav(&root.join("HOME/Odyssey/01 Intro.wav"), 900);
        fs::remove_file(root.join("Woodkid/S16/01 Goliath.wav")).expect("rm");
        write_wav(&root.join("Woodkid/S16/02 Run Boy Run.wav"), 300);

        let again = rescan_with_covers_progress(
            root,
            &paths::Dirs::inside(root).covers_dir(),
            marked(&first),
            |_| {},
        );
        assert_eq!(again.track_count(), 3);
        assert_eq!(
            genre_of(&again, "HOME/Odyssey/02 - Resonance.wav").as_deref(),
            Some("FROM THE LAST SCAN"),
            "an unchanged file is taken over without being opened"
        );
        let intro = again
            .track(TrackId::for_rel_path("HOME/Odyssey/01 Intro.wav"))
            .expect("the rewritten track is still listed");
        assert_eq!(intro.genre, None, "a changed file is read again");
        assert!(intro.duration.as_millis() >= 850, "with its new duration");
        assert!(
            again
                .track(TrackId::for_rel_path("Woodkid/S16/01 Goliath.wav"))
                .is_none(),
            "a deleted file leaves the library"
        );
        assert_eq!(
            genre_of(&again, "Woodkid/S16/02 Run Boy Run.wav"),
            None,
            "a new file is read"
        );
    }

    /// Size alone is not enough: a re-tag that happens to keep the size still moves the
    /// mtime, and that must be enough to read the file again.
    #[test]
    fn a_rescan_rereads_a_file_whose_mtime_moved_at_the_same_size() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let file = root.join("HOME/Odyssey/01 Intro.wav");
        write_wav(&file, 300);
        set_mtime(
            &file,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000),
        );
        let first = scan(root);

        set_mtime(
            &file,
            SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000),
        );
        let again = rescan_with_covers_progress(
            root,
            &paths::Dirs::inside(root).covers_dir(),
            marked(&first),
            |_| {},
        );
        assert_eq!(genre_of(&again, "HOME/Odyssey/01 Intro.wav"), None);
        assert_eq!(
            again.tracks().next().expect("track").added_at,
            SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000)
        );
    }

    /// Known tracks are hints, never facts: one that lies about a file that has since been
    /// corrupted is dropped with the file, exactly as a full scan would drop it.
    #[test]
    fn a_rescan_still_skips_a_file_that_became_unreadable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let file = root.join("HOME/Odyssey/01 Intro.wav");
        write_wav(&file, 300);
        let first = scan(root);
        fs::write(&file, b"NOT A WAV ANY MORE").expect("corrupt");

        let again = rescan_with_covers_progress(
            root,
            &paths::Dirs::inside(root).covers_dir(),
            marked(&first),
            |_| {},
        );
        assert!(
            again.is_empty(),
            "the corrupted file is skipped, not reused"
        );
    }

    /// A `chmod` moves neither size nor mtime, so an unreadable file would sail through the
    /// stamp check and stay in the library as a track that cannot be played.
    #[cfg(unix)]
    #[test]
    fn a_rescan_does_not_reuse_a_file_that_lost_its_read_permission() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let file = root.join("HOME/Odyssey/01 Intro.wav");
        write_wav(&file, 300);
        write_wav(&root.join("HOME/Odyssey/02 Outro.wav"), 300);
        let first = scan(root);
        assert_eq!(first.track_count(), 2);

        fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).expect("chmod");
        let again = rescan_with_covers_progress(
            root,
            &paths::Dirs::inside(root).covers_dir(),
            marked(&first),
            |_| {},
        );
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("chmod back");

        // Read again, not taken over: a full scan drops it (or, as root, which can open
        // anything, lists it with freshly read tags — never with the marked ones).
        assert_ne!(
            genre_of(&again, "HOME/Odyssey/01 Intro.wav").as_deref(),
            Some("FROM THE LAST SCAN")
        );
        assert_eq!(
            genre_of(&again, "HOME/Odyssey/02 Outro.wav").as_deref(),
            Some("FROM THE LAST SCAN"),
            "a readable neighbour is still reused"
        );
    }

    /// A scan the app gave up on must not touch the shared cover cache any more.
    #[test]
    fn a_cancelled_scan_stops_writing_covers() {
        let music = tempfile::tempdir().expect("tempdir");
        let data = tempfile::tempdir().expect("tempdir");
        let covers = data.path().join("covers");
        let root = music.path();
        write_wav_with_cover(&root.join("HOME/Odyssey/01 Intro.wav"), 300, 100);
        write_wav_with_cover(&root.join("Woodkid/S16/01 Goliath.wav"), 300, 120);
        let cover_files = || cache_files(&covers).len();

        let lib = rescan_cancellable(root, &covers, Vec::new(), || true, |_| {});
        assert_eq!(
            cover_files(),
            0,
            "cancelled before it began: no cover cache"
        );
        assert_eq!(lib.track_count(), 0, "…and no more reading either");

        // Cancelled after the first file: its cover may still land (a writer may hold it
        // already), the second file is never read.
        let cancel = std::cell::Cell::new(false);
        let lib = rescan_cancellable(
            root,
            &covers,
            Vec::new(),
            || cancel.get(),
            |p| {
                if p.phase == ScanPhase::Reading {
                    cancel.set(true);
                }
            },
        );
        assert_eq!(lib.track_count(), 1);
        assert!(cover_files() <= 1, "{:?}", cache_files(&covers));

        // Not cancelled: both.
        let full = scan_with_covers(root, &covers);
        assert_eq!(cover_files(), 2);

        // Cancelled while restoring a cleared cache: at most the cover in hand.
        for name in cache_files(&covers) {
            fs::remove_file(covers.join(name)).expect("clear the cache");
        }
        let cancel = std::cell::Cell::new(false);
        rescan_cancellable(
            root,
            &covers,
            full.tracks().cloned(),
            || cancel.get(),
            |p| {
                if p.phase == ScanPhase::Artwork {
                    cancel.set(true);
                }
            },
        );
        assert!(cover_files() <= 1, "{:?}", cache_files(&covers));
    }

    /// A cancelled scan's library is incomplete, so pruning by it would delete covers other
    /// libraries still use: it never prunes, however late it is cancelled.
    #[test]
    fn a_cancelled_scan_does_not_prune() {
        let music = tempfile::tempdir().expect("tempdir");
        let covers = tempfile::tempdir().expect("tempdir");
        let root = music.path();
        write_wav_with_cover(&root.join("HOME/Odyssey/01 Intro.wav"), 300, 100);
        // Another library's cover, written long before this scan.
        let other = covers.path().join("00000000000000ab.png");
        write_cover_png(&png_bytes(8), &other).expect("other cover");
        set_mtime(
            &other,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000),
        );

        // Cancelled once the walk has read the only file: just before the prune.
        let cancel = std::cell::Cell::new(false);
        let lib = rescan_cancellable(
            root,
            covers.path(),
            Vec::new(),
            || cancel.get(),
            |p| {
                if p.phase == ScanPhase::Reading {
                    cancel.set(true);
                }
            },
        );
        assert_eq!(lib.track_count(), 1, "the walk itself was complete");
        assert!(other.exists(), "a cancelled scan must not prune");

        // The same scan, not cancelled, does.
        scan_with_covers(root, covers.path());
        assert!(!other.exists());
    }

    /// End to end with the snapshot: what a scan saves, the next launch loads, and a
    /// rescan seeded with it reuses every track without reading the files.
    #[test]
    fn a_rescan_seeded_from_a_snapshot_changes_nothing() {
        let dir = seeded_root();
        let data = tempfile::tempdir().expect("tempdir");
        let dirs = paths::Dirs::at(data.path());
        let root = dir.path();
        let scanned = scan_with_covers(root, &dirs.covers_dir());
        crate::snapshot::save(&dirs.library_path(), &scanned).expect("save");

        let loaded = crate::snapshot::load(&dirs.library_path(), root, &dirs.covers_dir())
            .expect("a fresh snapshot loads");
        assert!(scanned.changed_albums(&loaded).is_empty());
        let rescanned =
            rescan_with_covers_progress(root, &dirs.covers_dir(), loaded.tracks().cloned(), |_| {});
        assert!(
            loaded.changed_albums(&rescanned).is_empty(),
            "nothing on disk moved, so nothing in the library may"
        );
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
