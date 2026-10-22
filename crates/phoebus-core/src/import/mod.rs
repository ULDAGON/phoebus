//! Bringing new songs into the library: what was dropped, where it goes, and how a later
//! edit of its fields or cover reaches the file.
//!
//! An import behaves like dropping the same files onto Apple Music, and there are two ways
//! to get that, chosen per library root by [`Mode::for_root`]:
//!
//! * [`Mode::MusicApp`] — macOS, with the root inside an Apple Music media folder. The
//!   Music app is the owner of that folder *and* of a database Phoebus never sees, so a
//!   file copied in behind its back would play in Phoebus and be missing from Music.
//!   Every import and every edit is therefore handed to the Music app itself
//!   ([`music_app`]): it copies the file, files it under its own organisation, writes the
//!   tags, and both libraries agree afterwards.
//! * [`Mode::Copy`] — Linux, or any other root on macOS. Phoebus is the only owner, so it
//!   files a copy as `<root>/<Album Artist>/<Album>/<NN Title>.<ext>` itself and writes
//!   edits into that copy's tags with `lofty` ([`copy`]).
//!
//! Either way the dropped original is never written to: an import copies, and every edit
//! goes to the copy. The one case where the Music app does *not* copy — its "Copy files to
//! Music Media folder" setting is off, and the track points at the dropped file itself —
//! is refused for edits ([`apply`]), because there an edit would rewrite the original.
//!
//! Everything here blocks (file copies, `osascript`), so the app calls it from a worker
//! thread, never from a frame.

pub mod copy;
pub mod music_app;

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use lofty::file::TaggedFileExt;
use lofty::prelude::{Accessor, ItemKey};
use walkdir::WalkDir;

use crate::model::{UNKNOWN_ALBUM, UNKNOWN_ARTIST, UNKNOWN_TITLE, split_track_prefix};
use crate::scanner;

/// The image formats a cover may be set from. Both the Music app and every tag format
/// `lofty` writes accept exactly these two.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg"];

/// The editable fields of one song — what the import list shows and the editor changes.
///
/// Strings are stored as typed, and an empty string means "not set" (for the title, the
/// editor refuses it). Year and track number of `None` clear the value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fields {
    /// Song title.
    pub title: String,
    /// Track artist.
    pub artist: String,
    /// Album title.
    pub album: String,
    /// Album artist — what the library groups albums by.
    pub album_artist: String,
    /// Genre.
    pub genre: String,
    /// Release year.
    pub year: Option<u32>,
    /// Track number on the album.
    pub track_no: Option<u32>,
}

/// One of the seven editable fields — what an edit names when it says what changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    /// [`Fields::title`].
    Title,
    /// [`Fields::artist`].
    Artist,
    /// [`Fields::album`].
    Album,
    /// [`Fields::album_artist`].
    AlbumArtist,
    /// [`Fields::genre`].
    Genre,
    /// [`Fields::year`].
    Year,
    /// [`Fields::track_no`].
    TrackNo,
}

impl Field {
    /// Every field, in editor order.
    pub const ALL: [Field; 7] = [
        Field::Title,
        Field::Artist,
        Field::Album,
        Field::AlbumArtist,
        Field::Genre,
        Field::Year,
        Field::TrackNo,
    ];
}

impl Fields {
    /// The fields that differ between `self` and `new`.
    ///
    /// An edit writes exactly these and nothing else. [`Fields::read`] fills gaps with
    /// fallbacks (album artist ← artist, title and track ← the file name) that were never
    /// in the tags; writing them back unasked would, on a compilation, give one track an
    /// album artist of its own and pull it out of its album in the Music app.
    pub fn diff(&self, new: &Fields) -> Vec<Field> {
        Field::ALL
            .into_iter()
            .filter(|field| match field {
                Field::Title => self.title != new.title,
                Field::Artist => self.artist != new.artist,
                Field::Album => self.album != new.album,
                Field::AlbumArtist => self.album_artist != new.album_artist,
                Field::Genre => self.genre != new.genre,
                Field::Year => self.year != new.year,
                Field::TrackNo => self.track_no != new.track_no,
            })
            .collect()
    }

    /// Read the fields of an audio file, with the scanner's fallbacks: album artist ←
    /// artist, title and track number ← the file name.
    ///
    /// Unlike the scanner, the directories are *not* used for artist and album: a dropped
    /// file usually sits in `Downloads/`, and an album called `Downloads` is worse than an
    /// empty field the user can see and fill in.
    ///
    /// Fails when `lofty` cannot read the file as audio at all.
    pub fn read(path: &Path) -> Result<Fields, Failure> {
        let tagged = lofty::read_from_path(path)
            .map_err(|e| Failure::new("NOT READABLE AS AUDIO", e.to_string()))?;
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (track_no, title) = split_track_prefix(&stem);
        let mut fields = Fields {
            title: title.to_string(),
            track_no,
            ..Fields::default()
        };
        if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
            if let Some(v) = scanner::text(tag.title().as_deref()) {
                fields.title = v;
            }
            fields.artist = scanner::text(tag.artist().as_deref()).unwrap_or_default();
            fields.album = scanner::text(tag.album().as_deref()).unwrap_or_default();
            fields.album_artist = scanner::text(tag.get_string(ItemKey::AlbumArtist))
                .unwrap_or_else(|| fields.artist.clone());
            fields.genre = scanner::text(tag.genre().as_deref()).unwrap_or_default();
            if let Some(n) = tag.track() {
                fields.track_no = Some(n);
            }
            fields.year = tag
                .date()
                .map(|d| u32::from(d.year))
                .or_else(|| {
                    scanner::text(tag.get_string(ItemKey::RecordingDate))
                        .and_then(|s| scanner::parse_year(&s))
                })
                .or_else(|| {
                    scanner::text(tag.get_string(ItemKey::Year))
                        .and_then(|s| scanner::parse_year(&s))
                });
        }
        if fields.title.trim().is_empty() {
            fields.title = UNKNOWN_TITLE.to_string();
        }
        Ok(fields)
    }

    /// The album artist a file is filed under: the album artist, else the artist, else
    /// [`UNKNOWN_ARTIST`] — the same order the scanner groups by.
    pub fn filing_artist(&self) -> &str {
        [self.album_artist.trim(), self.artist.trim()]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or(UNKNOWN_ARTIST)
    }

    /// The album a file is filed under, or [`UNKNOWN_ALBUM`].
    pub fn filing_album(&self) -> &str {
        match self.album.trim() {
            "" => UNKNOWN_ALBUM,
            album => album,
        }
    }
}

/// How an import reaches the library. See the module documentation for why there are two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Hand the file to the Music app (macOS, Apple Music media folder).
    MusicApp,
    /// Copy the file into the root and write tags with `lofty`.
    Copy,
}

impl Mode {
    /// The mode for a library root on this platform.
    ///
    /// Blocking — it resolves the root on disk, and on macOS reads the Music app's
    /// preferences (once per process, see [`music_app::media_folder`]). Call it when the
    /// root changes, not on every frame.
    pub fn for_root(root: &Path) -> Mode {
        let macos = cfg!(target_os = "macos");
        let media = if macos {
            music_app::media_folder()
        } else {
            None
        };
        Mode::choose(macos, root, media.as_deref())
    }

    /// [`Mode::for_root`] with the platform and the Music app's media folder as
    /// parameters, so every answer is testable on either platform.
    ///
    /// Both paths are resolved first, so a root reached through a symlink is recognised.
    /// When the Music app's configured media folder is known, it is the whole answer: the
    /// root is in Music mode exactly when it is that folder or inside it — a custom
    /// location is found, and a stale `iTunes Media` backup is not mistaken for the live
    /// library. Only when the preference cannot be read does the folder-name heuristic
    /// ([`is_music_media_folder`]) decide.
    pub fn choose(macos: bool, root: &Path, media_folder: Option<&Path>) -> Mode {
        if !macos {
            return Mode::Copy;
        }
        let root = canonical(root);
        let music = match media_folder {
            Some(media) => root.starts_with(canonical(media)),
            None => is_music_media_folder(&root),
        };
        if music { Mode::MusicApp } else { Mode::Copy }
    }
}

/// `path` with symlinks and `..` resolved, or as given when it cannot be (it does not
/// exist).
fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// True if `root` looks like an Apple Music (or iTunes) media folder, or a folder inside
/// one — the fallback for when the Music app's own preference cannot be read.
///
/// It goes by the names the Music app gives the folder: `…/Music/Media.localized` (shown as
/// `Media`), `…/Music/Media` when the folder was never localised, and `…/iTunes/iTunes
/// Media` from the iTunes days. The `Music` folder the songs actually live in sits one level
/// below. A name match alone is not taken for `Media`, which is common enough to be a
/// coincidence; it also needs the `Music` library folder as its parent.
pub fn is_music_media_folder(root: &Path) -> bool {
    let names: Vec<String> = root
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    names.iter().enumerate().any(|(i, name)| {
        let parent = i
            .checked_sub(1)
            .and_then(|p| names.get(p))
            .map(String::as_str);
        match name.as_str() {
            "Media.localized" | "iTunes Media" => true,
            "Media" => parent == Some("Music"),
            _ => false,
        }
    })
}

/// What one dropped path is, by its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// One of [`scanner::AUDIO_EXTENSIONS`].
    Audio,
    /// One of [`IMAGE_EXTENSIONS`] — a cover candidate.
    Image,
    /// Anything else.
    Other,
}

/// Classify a file by its extension, case-insensitively.
pub fn kind_of(path: &Path) -> Kind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if scanner::AUDIO_EXTENSIONS.contains(&ext.as_str()) {
        Kind::Audio
    } else if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        Kind::Image
    } else {
        Kind::Other
    }
}

/// A drop, sorted: what to import, what could be a cover, and what is neither.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dropped {
    /// Audio files, the dropped ones first in drop order, then each folder's in name order.
    pub audio: Vec<PathBuf>,
    /// Images dropped *as files*. An image inside a dropped folder is an album's
    /// `cover.jpg`, not a request to change a cover, so it counts as skipped.
    pub images: Vec<PathBuf>,
    /// Everything else, folders' contents included: reported, never imported.
    pub skipped: Vec<PathBuf>,
}

/// Sort what was dropped, recursing into folders the way the scanner walks the library:
/// hidden entries are passed over, symlinks are not followed, and the depth is capped.
pub fn sort_drop(paths: &[PathBuf]) -> Dropped {
    let mut out = Dropped::default();
    for path in paths {
        if path.is_dir() {
            let walker = WalkDir::new(path)
                .max_depth(scanner::MAX_DEPTH)
                .follow_links(false)
                .sort_by_file_name()
                .into_iter()
                .filter_entry(|e| e.depth() == 0 || !scanner::name_is_hidden(e.file_name()));
            for entry in walker.flatten() {
                if !entry.file_type().is_file() {
                    continue;
                }
                match kind_of(entry.path()) {
                    Kind::Audio => out.audio.push(entry.into_path()),
                    Kind::Image | Kind::Other => out.skipped.push(entry.into_path()),
                }
            }
            continue;
        }
        match kind_of(path) {
            Kind::Audio => out.audio.push(path.clone()),
            Kind::Image => out.images.push(path.clone()),
            Kind::Other => out.skipped.push(path.clone()),
        }
    }
    out
}

/// True if `path` is `root` or inside it, after resolving symlinks and `..` on both.
///
/// Canonical paths, because the Music folder is commonly reached through a symlink and
/// `~/Music/Music/Media.localized` is the same directory however it was spelled. A path
/// that cannot be resolved (it does not exist) is compared as given.
pub fn is_inside(root: &Path, path: &Path) -> bool {
    canonical(path).starts_with(canonical(root))
}

/// Why an import or an edit did not happen.
///
/// `reason` is the short, upper-case line the import list shows in its status column
/// (`MUSIC APP REFUSED`); `detail` is the whole story for its tooltip and the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// Short status line.
    pub reason: String,
    /// The underlying error, verbatim.
    pub detail: String,
}

impl Failure {
    /// A failure with a short `reason` and the error behind it.
    pub fn new(reason: impl Into<String>, detail: impl Into<String>) -> Failure {
        Failure {
            reason: reason.into(),
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.detail.is_empty() {
            f.write_str(&self.reason)
        } else {
            write!(f, "{}: {}", self.reason, self.detail)
        }
    }
}

/// Where an imported song ended up, and the handle a later edit needs to reach it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    /// Which way it went in — and therefore which way an edit has to go.
    pub mode: Mode,
    /// The file as filed now. Always set in [`Mode::Copy`]; in [`Mode::MusicApp`] it is
    /// what the Music app reported, which it can leave unset for a track it has not
    /// finished copying.
    pub location: Option<PathBuf>,
    /// The Music app's persistent ID of the track ([`Mode::MusicApp`] only).
    pub music_id: Option<String>,
}

impl Placed {
    /// True if the Music app kept the file outside `root`.
    ///
    /// That happens when "Copy files to Music Media folder when adding to library" is off
    /// in Music's settings: the track is in Music, but it points at the original file, so
    /// a scan of the media folder will not find it.
    pub fn outside(&self, root: &Path) -> bool {
        self.location
            .as_deref()
            .is_some_and(|l| !is_inside(root, l))
    }
}

/// What an import came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The song is in the library now.
    Imported(Placed),
    /// The file already lives inside the library root; importing it would only make a
    /// duplicate.
    AlreadyInLibrary,
    /// The Music app did not answer in time. It may still be adding the file — it usually
    /// is — so this is neither a success nor a failure, and the file must not be offered to
    /// it again (that would add it twice).
    Uncertain(Failure),
}

/// Import one audio file into the library at `root`, by `mode`.
///
/// `fields` are the file's own (from [`Fields::read`]); [`Mode::Copy`] files the copy by
/// them, [`Mode::MusicApp`] lets the Music app read the tags itself.
pub fn import(mode: Mode, root: &Path, src: &Path, fields: &Fields) -> Result<Outcome, Failure> {
    if is_inside(root, src) {
        return Ok(Outcome::AlreadyInLibrary);
    }
    if !root.is_dir() {
        return Err(Failure::new(
            "LIBRARY ROOT MISSING",
            format!("{} is not a directory", root.display()),
        ));
    }
    let placed = match mode {
        Mode::MusicApp => {
            if !music_app::accepts(src) {
                let ext = src
                    .extension()
                    .map(|e| e.to_string_lossy().to_ascii_uppercase())
                    .unwrap_or_default();
                return Err(Failure::new(
                    format!("MUSIC APP CANNOT PLAY {ext}"),
                    "the Music app only adds AAC/ALAC (m4a), MP3, WAV and AIFF files",
                ));
            }
            let track = match music_app::add(src) {
                Ok(track) => track,
                Err(e) if music_app::timed_out(&e) => return Ok(Outcome::Uncertain(e)),
                Err(e) => return Err(e),
            };
            Placed {
                mode,
                location: track.location,
                music_id: Some(track.persistent_id),
            }
        }
        Mode::Copy => Placed {
            mode,
            location: Some(copy::place(root, src, fields)?),
            music_id: None,
        },
    };
    Ok(Outcome::Imported(placed))
}

/// An edit of an imported song: its fields as they should be afterwards, which of them
/// the user actually changed, and a new cover image, if any.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edit {
    /// All seven fields as the editor shows them.
    pub fields: Fields,
    /// The ones to write ([`Fields::diff`] against what was shown before). Only these
    /// reach the file or the Music app.
    pub changed: Vec<Field>,
    /// A PNG or JPEG to make the cover.
    pub cover: Option<PathBuf>,
}

/// How far an edit got. An edit is two or three steps — fields, cover, and in copy mode a
/// move — and a failure in a later one does not undo an earlier one, so the answer has to
/// say what landed as well as what did not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    /// Where the song is now.
    pub placed: Placed,
    /// The changed fields were written.
    pub fields: bool,
    /// The cover was written.
    pub cover: bool,
    /// What stopped the rest, if anything did.
    pub failure: Option<Failure>,
}

impl Applied {
    /// True if anything reached the file — the cue for a rescan.
    pub fn anything(&self) -> bool {
        self.fields || self.cover
    }
}

/// Apply an edit to an imported song, the way it went in. Every step that succeeded is
/// reported as such even when a later one fails ([`Applied`]).
pub fn apply(placed: &Placed, root: &Path, edit: &Edit) -> Applied {
    let mut out = Applied {
        placed: placed.clone(),
        fields: false,
        cover: false,
        failure: None,
    };
    if let Err(failure) = apply_steps(&mut out, root, edit) {
        out.failure = Some(failure);
    }
    out
}

fn apply_steps(out: &mut Applied, root: &Path, edit: &Edit) -> Result<(), Failure> {
    let image = edit.cover.as_deref().map(read_cover).transpose()?;
    let wants_fields = !edit.changed.is_empty();
    match out.placed.mode {
        Mode::MusicApp => {
            if out.placed.outside(root) {
                return Err(Failure::new(
                    "ORIGINAL NOT COPIED",
                    "the Music app refers to the dropped file itself, so an edit would change the original",
                ));
            }
            let id = out
                .placed
                .music_id
                .clone()
                .ok_or_else(|| Failure::new("NOT IN THE MUSIC APP", "no persistent ID"))?;
            if wants_fields {
                let track = music_app::set_fields(&id, &edit.fields, &edit.changed)?;
                out.placed.location = track.location.or(out.placed.location.take());
                out.fields = true;
            }
            if let (Some(path), Some((_, kind))) = (&edit.cover, &image) {
                let track = music_app::set_artwork(&id, path, *kind)?;
                out.placed.location = track.location.or(out.placed.location.take());
                out.cover = true;
            }
        }
        Mode::Copy => {
            let location = out
                .placed
                .location
                .clone()
                .ok_or_else(|| Failure::new("FILE MISSING", "no location"))?;
            if !location.is_file() {
                return Err(Failure::new(
                    "FILE MISSING",
                    format!("{} is gone", location.display()),
                ));
            }
            if let Some((bytes, kind)) = &image {
                copy::write_cover(&location, bytes, *kind)?;
                out.cover = true;
            }
            if wants_fields {
                copy::write_fields(&location, &edit.fields, &edit.changed)?;
                out.fields = true;
                out.placed.location = Some(copy::refile(root, &location, &edit.fields)?);
            }
        }
    }
    Ok(())
}

/// The two cover formats, told apart by their bytes rather than their extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
}

impl ImageKind {
    /// Sniff the format from the file's magic bytes.
    pub fn sniff(bytes: &[u8]) -> Option<ImageKind> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(ImageKind::Png)
        } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(ImageKind::Jpeg)
        } else {
            None
        }
    }
}

/// Read a cover candidate and make sure it is a PNG or JPEG that actually decodes — a
/// truncated download must not end up embedded in somebody's song.
pub fn read_cover(path: &Path) -> Result<(Vec<u8>, ImageKind), Failure> {
    let bytes = fs::read(path).map_err(|e| Failure::new("IMAGE NOT READABLE", e.to_string()))?;
    let kind = ImageKind::sniff(&bytes).ok_or_else(|| {
        Failure::new(
            "NOT A PNG OR JPEG",
            format!("{} is not a PNG or JPEG image", path.display()),
        )
    })?;
    image::load_from_memory(&bytes)
        .map_err(|e| Failure::new("IMAGE DOES NOT DECODE", e.to_string()))?;
    Ok((bytes, kind))
}

/// The first picture embedded in an audio file, whatever its type — what the import list
/// shows as the song's cover before it is in the library.
pub fn embedded_picture(path: &Path) -> Option<Vec<u8>> {
    scanner::first_picture(path)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;

    /// The scanner's minimal 8 kHz mono WAV: real enough for `lofty`, no tags at all.
    pub(crate) fn write_wav(path: &Path) {
        let sample_rate: u32 = 8000;
        let data_len = sample_rate / 5 * 2;
        let mut b: Vec<u8> = Vec::with_capacity(44 + data_len as usize);
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&sample_rate.to_le_bytes());
        b.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        b.resize(44 + data_len as usize, 0);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, &b).expect("write wav");
    }

    /// A WAV with an ID3v2 tag carrying `fields`.
    pub(crate) fn write_tagged_wav(path: &Path, fields: &Fields) {
        write_wav(path);
        copy::write_fields(path, fields, &Field::ALL).expect("tag the fixture");
    }

    /// An encoded image of `edge` px in `format`.
    pub(crate) fn image_bytes(edge: u32, format: image::ImageFormat) -> Vec<u8> {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(edge, edge, |x, y| {
            image::Rgb([(x * 7 % 256) as u8, (y * 3 % 256) as u8, 90])
        }));
        let mut out: Vec<u8> = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), format)
            .expect("encode image");
        out
    }

    pub(crate) fn sample() -> Fields {
        Fields {
            title: "Resonance".into(),
            artist: "HOME".into(),
            album: "Odyssey".into(),
            album_artist: "HOME".into(),
            genre: "Synthwave".into(),
            year: Some(2014),
            track_no: Some(3),
        }
    }

    #[test]
    fn fields_fall_back_to_the_file_name_not_to_the_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("Downloads/07 - Night Drive.wav");
        write_wav(&path);
        let fields = Fields::read(&path).expect("readable");
        assert_eq!(fields.title, "Night Drive");
        assert_eq!(fields.track_no, Some(7));
        assert_eq!(fields.artist, "", "a download folder is not an artist");
        assert_eq!(fields.album, "", "…nor an album");
        assert_eq!(fields.filing_artist(), UNKNOWN_ARTIST);
        assert_eq!(fields.filing_album(), UNKNOWN_ALBUM);
    }

    #[test]
    fn fields_read_back_what_the_tags_say() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("x.wav");
        write_tagged_wav(&path, &sample());
        assert_eq!(Fields::read(&path).expect("readable"), sample());
    }

    #[test]
    fn a_file_that_is_not_audio_is_refused_with_a_short_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fake.mp3");
        fs::write(&path, b"plainly not an mp3").expect("write");
        let err = Fields::read(&path).expect_err("not audio");
        assert_eq!(err.reason, "NOT READABLE AS AUDIO");
    }

    #[test]
    fn the_album_artist_is_what_a_file_is_filed_under() {
        let mut f = sample();
        f.album_artist = "  ".into();
        assert_eq!(f.filing_artist(), "HOME", "falls back to the artist");
        f.album_artist = "Various Artists".into();
        assert_eq!(f.filing_artist(), "Various Artists");
    }

    #[test]
    fn only_a_mac_with_a_music_media_folder_goes_through_the_music_app() {
        let music = Path::new("/Users/me/Music/Music/Media.localized/Music");
        assert_eq!(Mode::choose(true, music, None), Mode::MusicApp);
        assert_eq!(
            Mode::choose(false, music, None),
            Mode::Copy,
            "Linux has no Music app"
        );
        assert_eq!(
            Mode::choose(true, Path::new("/Users/me/.phoebus"), None),
            Mode::Copy
        );
    }

    /// The Music app's own media-folder preference decides when it is known: a custom
    /// location with an ordinary name is found, a symlinked root is resolved first, and a
    /// stale `iTunes Media` backup that merely *looks* like a media folder is not taken.
    #[test]
    fn the_configured_media_folder_outranks_the_folder_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        let media = dir.path().join("Audio Archive");
        let root = media.join("Music");
        fs::create_dir_all(&root).expect("mkdir");
        let link = dir.path().join("link-to-root");
        std::os::unix::fs::symlink(&root, &link).expect("symlink");
        let stale = dir.path().join("Backup/iTunes/iTunes Media/Music");
        fs::create_dir_all(&stale).expect("mkdir");

        assert_eq!(Mode::choose(true, &root, Some(&media)), Mode::MusicApp);
        assert_eq!(Mode::choose(true, &media, Some(&media)), Mode::MusicApp);
        assert_eq!(
            Mode::choose(true, &link, Some(&media)),
            Mode::MusicApp,
            "a symlinked root is the same folder"
        );
        assert_eq!(
            Mode::choose(true, &stale, Some(&media)),
            Mode::Copy,
            "a backup is not the live library"
        );
        // No preference to go by: the names decide, which is all they are good for.
        assert_eq!(Mode::choose(true, &stale, None), Mode::MusicApp);
        assert_eq!(Mode::choose(true, &root, None), Mode::Copy);
        assert_eq!(Mode::choose(false, &root, Some(&media)), Mode::Copy);
    }

    #[test]
    fn a_diff_names_only_what_changed() {
        let before = sample();
        assert!(before.diff(&before).is_empty());
        let after = Fields {
            genre: "Ambient".into(),
            year: None,
            ..sample()
        };
        assert_eq!(before.diff(&after), vec![Field::Genre, Field::Year]);
    }

    #[test]
    fn music_media_folders_are_recognised_by_their_names() {
        for yes in [
            "/Users/me/Music/Music/Media.localized/Music",
            "/Users/me/Music/Music/Media.localized",
            "/Users/me/Music/Music/Media/Music",
            "/Volumes/Ext/iTunes/iTunes Media/Music",
        ] {
            assert!(is_music_media_folder(Path::new(yes)), "{yes}");
        }
        for no in [
            "/Users/me/.phoebus",
            "/Users/me/Music",
            "/Users/me/Projects/Media/Music",
            "/srv/Media",
        ] {
            assert!(!is_music_media_folder(Path::new(no)), "{no}");
        }
    }

    #[test]
    fn a_drop_is_sorted_and_folders_are_walked_like_the_scanner_walks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let album = root.join("Album");
        write_wav(&album.join("02 B.wav"));
        write_wav(&album.join("01 A.WAV"));
        write_wav(&album.join("CD2/01 C.flac"));
        write_wav(&album.join(".hidden/secret.wav"));
        fs::write(album.join("cover.jpg"), b"jpg").expect("write");
        fs::write(album.join("notes.txt"), b"txt").expect("write");
        let single = root.join("single.mp3");
        let picture = root.join("art.PNG");
        let readme = root.join("readme.md");
        for p in [&single, &picture, &readme] {
            fs::write(p, b"x").expect("write");
        }

        let sorted = sort_drop(&[
            single.clone(),
            album.clone(),
            picture.clone(),
            readme.clone(),
        ]);
        assert_eq!(
            sorted.audio,
            vec![
                single,
                album.join("01 A.WAV"),
                album.join("02 B.wav"),
                album.join("CD2/01 C.flac"),
            ],
            "drop order, then name order inside a folder; hidden entries are passed over"
        );
        assert_eq!(sorted.images, vec![picture]);
        assert_eq!(
            sorted.skipped,
            vec![album.join("cover.jpg"), album.join("notes.txt"), readme],
            "a folder's cover.jpg is not a request to change a cover"
        );
    }

    #[test]
    fn inside_the_root_is_judged_on_resolved_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("lib");
        write_wav(&root.join("A/B/x.wav"));
        let outside = dir.path().join("elsewhere.wav");
        write_wav(&outside);
        assert!(is_inside(&root, &root.join("A/B/x.wav")));
        assert!(is_inside(&root, &root.join("A/../A/B/x.wav")));
        assert!(!is_inside(&root, &outside));
        // The same directory reached through a symlink is still the library.
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&root, &link).expect("symlink");
        assert!(is_inside(&root, &link.join("A/B/x.wav")));
    }

    #[test]
    fn importing_a_file_from_the_library_itself_is_a_no_op() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let inside = root.join("HOME/Odyssey/01 Intro.wav");
        write_wav(&inside);
        let entries = || WalkDir::new(root).into_iter().flatten().count();
        let before = entries();
        let outcome = import(Mode::Copy, root, &inside, &sample()).expect("no failure");
        assert_eq!(outcome, Outcome::AlreadyInLibrary);
        assert_eq!(entries(), before, "nothing was copied");
        // …and the same holds for the Music app mode, which must not even be asked.
        let outcome = import(Mode::MusicApp, root, &inside, &sample()).expect("no failure");
        assert_eq!(outcome, Outcome::AlreadyInLibrary);
    }

    #[test]
    fn a_missing_root_is_refused_rather_than_created() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("in.wav");
        write_wav(&src);
        let root = dir.path().join("unmounted/drive");
        let err = import(Mode::Copy, &root, &src, &sample()).expect_err("no root");
        assert_eq!(err.reason, "LIBRARY ROOT MISSING");
        assert!(!root.exists(), "the root must not be conjured up");
    }

    /// The Music app plays neither FLAC nor Ogg. Asking it anyway would come back as a
    /// silent refusal; saying why up front is the better answer — and it is decided before
    /// `osascript` is ever started, which is what lets this run on any machine.
    #[test]
    fn the_music_app_is_not_asked_for_formats_it_cannot_play() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("Music/Media.localized/Music");
        fs::create_dir_all(&root).expect("mkdir");
        let src = dir.path().join("song.flac");
        fs::write(&src, b"fLaC").expect("write");
        let err = import(Mode::MusicApp, &root, &src, &sample()).expect_err("refused");
        assert_eq!(err.reason, "MUSIC APP CANNOT PLAY FLAC");
    }

    #[test]
    fn copy_mode_imports_and_edits_end_to_end() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("lib");
        fs::create_dir_all(&root).expect("mkdir");
        let src = dir.path().join("Downloads/track.wav");
        write_tagged_wav(&src, &sample());
        let original = fs::read(&src).expect("read");

        let fields = Fields::read(&src).expect("readable");
        let Outcome::Imported(placed) = import(Mode::Copy, &root, &src, &fields).expect("ok")
        else {
            panic!("not imported");
        };
        let at = placed.location.clone().expect("a location");
        assert_eq!(at, root.join("HOME/Odyssey/03 Resonance.wav"));
        assert!(!placed.outside(&root));

        // Edit: new album and a cover. The file follows its album, the old folders go,
        // and the dropped original is never touched.
        let cover = dir.path().join("cover.png");
        fs::write(&cover, image_bytes(8, image::ImageFormat::Png)).expect("write");
        let edited = Fields {
            album: "Resonance (Single)".into(),
            ..fields.clone()
        };
        let applied = apply(
            &placed,
            &root,
            &Edit {
                changed: fields.diff(&edited),
                fields: edited.clone(),
                cover: Some(cover.clone()),
            },
        );
        assert_eq!(applied.failure, None);
        assert!(applied.fields && applied.cover);
        let placed = applied.placed;
        let moved = placed.location.clone().expect("a location");
        assert_eq!(moved, root.join("HOME/Resonance (Single)/03 Resonance.wav"));
        assert!(!at.exists() && !root.join("HOME/Odyssey").exists());
        assert_eq!(Fields::read(&moved).expect("readable"), edited);
        assert_eq!(
            embedded_picture(&moved),
            Some(fs::read(&cover).expect("read"))
        );
        assert_eq!(
            fs::read(&src).expect("read"),
            original,
            "the original is untouched"
        );

        // The scanner sees exactly what the editor said.
        let library = crate::scanner::scan_with_covers(&root, &dir.path().join("covers"));
        assert_eq!(library.track_count(), 1);
        let track = library.track(library.tracks_sorted()[0]).expect("track");
        assert_eq!(track.album, "Resonance (Single)");
        assert!(track.has_artwork);
    }

    #[test]
    fn a_cover_must_be_a_real_png_or_jpeg() {
        let dir = tempfile::tempdir().expect("tempdir");
        let jpeg = dir.path().join("c.jpg");
        fs::write(&jpeg, image_bytes(4, image::ImageFormat::Jpeg)).expect("write");
        assert_eq!(read_cover(&jpeg).expect("ok").1, ImageKind::Jpeg);

        let fake = dir.path().join("fake.png");
        fs::write(&fake, b"GIF89a....").expect("write");
        assert_eq!(
            read_cover(&fake).expect_err("gif").reason,
            "NOT A PNG OR JPEG"
        );

        let truncated = dir.path().join("cut.png");
        let mut bytes = image_bytes(16, image::ImageFormat::Png);
        bytes.truncate(40);
        fs::write(&truncated, bytes).expect("write");
        assert_eq!(
            read_cover(&truncated).expect_err("cut").reason,
            "IMAGE DOES NOT DECODE"
        );
    }

    #[test]
    fn an_edit_of_a_vanished_copy_says_so() {
        let placed = Placed {
            mode: Mode::Copy,
            location: Some(PathBuf::from("/nonexistent/phoebus/x.wav")),
            music_id: None,
        };
        let edit = Edit {
            fields: sample(),
            changed: vec![Field::Title],
            cover: None,
        };
        let applied = apply(&placed, Path::new("/nonexistent"), &edit);
        assert_eq!(
            applied.failure.as_ref().expect("gone").reason,
            "FILE MISSING"
        );
        assert!(!applied.anything());
    }

    /// Only the changed fields are written: an untouched album artist that `Fields::read`
    /// filled in from the artist must not appear in the tags afterwards.
    #[test]
    fn an_edit_writes_only_the_fields_that_changed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("lib");
        fs::create_dir_all(&root).expect("mkdir");
        let src = dir.path().join("in.wav");
        // Tagged with an artist and no album artist: the read fills one in.
        write_wav(&src);
        copy::write_fields(
            &src,
            &Fields {
                title: "Song".into(),
                artist: "Guest".into(),
                ..Fields::default()
            },
            &[Field::Title, Field::Artist],
        )
        .expect("tag");
        let shown = Fields::read(&src).expect("readable");
        assert_eq!(shown.album_artist, "Guest", "the fallback the editor shows");
        let Outcome::Imported(placed) = import(Mode::Copy, &root, &src, &shown).expect("ok") else {
            panic!("not imported");
        };
        let edited = Fields {
            genre: "Jazz".into(),
            ..shown.clone()
        };
        let applied = apply(
            &placed,
            &root,
            &Edit {
                changed: shown.diff(&edited),
                fields: edited,
                cover: None,
            },
        );
        assert_eq!(applied.failure, None);
        let at = applied.placed.location.expect("location");
        let tagged = lofty::read_from_path(&at).expect("read");
        let tag = tagged.primary_tag().expect("tag");
        assert_eq!(tag.genre().as_deref(), Some("Jazz"));
        assert_eq!(
            tag.get_string(ItemKey::AlbumArtist),
            None,
            "the fallback was never written"
        );
    }

    /// Tags written and then the move refused: the edit reports both halves, so the list
    /// can keep the new fields and still rescan.
    #[test]
    fn a_partial_edit_reports_what_landed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("lib");
        fs::create_dir_all(&root).expect("mkdir");
        let src = dir.path().join("in.wav");
        write_tagged_wav(&src, &sample());
        let Outcome::Imported(placed) = import(Mode::Copy, &root, &src, &sample()).expect("ok")
        else {
            panic!("not imported");
        };
        let edited = Fields {
            album_artist: "Somebody Else".into(),
            ..sample()
        };
        // The root refuses a new artist folder; the song's own folder stays writable.
        fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).expect("chmod");
        let applied = apply(
            &placed,
            &root,
            &Edit {
                changed: sample().diff(&edited),
                fields: edited.clone(),
                cover: None,
            },
        );
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("chmod back");
        assert!(applied.fields, "the tags were written");
        assert_eq!(
            applied.failure.as_ref().expect("refused").reason,
            "NOT MOVED"
        );
        assert_eq!(
            applied.placed.location, placed.location,
            "still where it was"
        );
        let at = applied.placed.location.expect("location");
        assert_eq!(Fields::read(&at).expect("readable"), edited);
    }

    /// A track the Music app kept outside its media folder points at the user's own file:
    /// an edit is refused before the Music app is ever asked.
    #[test]
    fn an_original_the_music_app_did_not_copy_is_never_edited() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("Music/Media.localized/Music");
        fs::create_dir_all(&root).expect("mkdir");
        let original = dir.path().join("Downloads/song.m4a");
        write_wav(&original);
        let placed = Placed {
            mode: Mode::MusicApp,
            location: Some(original),
            music_id: Some("0123456789ABCDEF".into()),
        };
        assert!(placed.outside(&root));
        let applied = apply(
            &placed,
            &root,
            &Edit {
                fields: sample(),
                changed: vec![Field::Title],
                cover: None,
            },
        );
        assert_eq!(
            applied.failure.as_ref().expect("refused").reason,
            "ORIGINAL NOT COPIED"
        );
        assert!(!applied.anything());
    }

    #[test]
    fn kinds_go_by_extension_case_insensitively() {
        assert_eq!(kind_of(Path::new("a/B.M4A")), Kind::Audio);
        assert_eq!(kind_of(Path::new("a/B.JPEG")), Kind::Image);
        assert_eq!(kind_of(Path::new("a/B.gif")), Kind::Other);
        assert_eq!(kind_of(Path::new("a/noext")), Kind::Other);
    }
}
