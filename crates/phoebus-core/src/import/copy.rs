//! [`Mode::Copy`](super::Mode::Copy): Phoebus files the song itself.
//!
//! The layout is Apple Music's — `<root>/<Album Artist>/<Album>/<NN Title>.<ext>` — so a
//! library built by Phoebus reads the same as one built by the Music app, and the scanner's
//! path fallbacks agree with the tags even for a reader that ignores tags.
//!
//! Two rules hold for every write here:
//!
//! * **Nothing is ever overwritten.** A name that is taken gets ` 2`, ` 3`, … before its
//!   extension, and the claim on the name is atomic (`create_new` for a copy, `hard_link`
//!   for a move), so two imports racing for one name cannot both win.
//! * **Only the copy is written.** Tags and covers go into the file under the root; the
//!   dropped original is opened for reading only.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::{Accessor, ItemKey};
use lofty::tag::Tag;
use lofty::tag::items::Timestamp;

use super::{Failure, Fields, ImageKind};
use crate::model::UNKNOWN_TITLE;

/// Longest file or folder name written, in bytes. Every file system Phoebus runs on allows
/// 255; the margin leaves room for the ` 99` of a collision and the extension.
const MAX_NAME_BYTES: usize = 200;

/// How many ` N` suffixes are tried before a name is given up on. A folder with a
/// thousand songs of one title is a bug somewhere else.
const MAX_SUFFIX: u32 = 1000;

/// Make `name` safe as one path component: no separators, no control characters, no
/// leading dot (the scanner skips hidden entries, so `.38 Special` would vanish), and not
/// longer than [`MAX_NAME_BYTES`]. Empty after all that becomes `fallback`.
///
/// `:` is replaced too. It is legal on both platforms, but Finder shows it as `/`, and the
/// Music app files a `:` as `_` — the same name has to land in the same folder whichever
/// app filed it.
pub fn sanitize(name: &str, fallback: &str) -> String {
    let mut out: String = name
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| {
            if matches!(c, '/' | ':' | '\\') {
                '_'
            } else {
                c
            }
        })
        .collect();
    out = out.trim().to_string();
    if out.starts_with('.') {
        out.replace_range(..1, "_");
    }
    if out.len() > MAX_NAME_BYTES {
        let mut end = MAX_NAME_BYTES;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out = out.trim_end().to_string();
    }
    if out.is_empty() {
        fallback.to_string()
    } else {
        out
    }
}

/// Where a song with `fields` belongs under `root`: `<Album Artist>/<Album>/<NN Title>.<ext>`.
///
/// The track number is two digits, as the Music app writes it, and absent when the song
/// has none. `ext` is lower-cased.
pub fn target(root: &Path, fields: &Fields, ext: &str) -> PathBuf {
    let artist = sanitize(fields.filing_artist(), crate::model::UNKNOWN_ARTIST);
    let album = sanitize(fields.filing_album(), crate::model::UNKNOWN_ALBUM);
    let title = sanitize(&fields.title, UNKNOWN_TITLE);
    let stem = match fields.track_no {
        Some(n) => format!("{n:02} {title}"),
        None => title,
    };
    let ext = ext.to_ascii_lowercase();
    let file = if ext.is_empty() {
        stem
    } else {
        format!("{stem}.{ext}")
    };
    root.join(artist).join(album).join(file)
}

/// The `n`-th candidate for `path`: the path itself, then `Name 2.ext`, `Name 3.ext`, ….
fn candidate(path: &Path, n: u32) -> PathBuf {
    if n <= 1 {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match path.extension() {
        Some(ext) => format!("{stem} {n}.{}", ext.to_string_lossy()),
        None => format!("{stem} {n}"),
    };
    path.with_file_name(name)
}

/// The extension of `path`, lower-cased, or empty.
fn ext_of(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Copy `src` into its place under `root` and return where it landed.
///
/// The copy's modification time is set to now: the scanner takes `added_at` from it, and a
/// song imported today belongs at the top of Recently Added, not wherever its original's
/// date would put it.
pub fn place(root: &Path, src: &Path, fields: &Fields) -> Result<PathBuf, Failure> {
    let wanted = target(root, fields, &ext_of(src));
    let dir = wanted.parent().unwrap_or(root);
    fs::create_dir_all(dir).map_err(|e| Failure::new("COPY FAILED", e.to_string()))?;
    let mut input =
        File::open(src).map_err(|e| Failure::new("SOURCE NOT READABLE", e.to_string()))?;
    for n in 1..=MAX_SUFFIX {
        let dest = candidate(&wanted, n);
        let mut output = match OpenOptions::new().write(true).create_new(true).open(&dest) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(Failure::new("COPY FAILED", e.to_string())),
        };
        let copied = io::copy(&mut input, &mut output)
            .and_then(|_| output.sync_all())
            .and_then(|()| output.set_modified(SystemTime::now()));
        if let Err(e) = copied {
            drop(output);
            // Never leave a half-copied song behind for the next scan to choke on.
            let _ = fs::remove_file(&dest);
            return Err(Failure::new("COPY FAILED", e.to_string()));
        }
        return Ok(dest);
    }
    Err(Failure::new(
        "COPY FAILED",
        format!("no free name for {}", wanted.display()),
    ))
}

/// Write `fields` into the tags of `path` (the library copy), creating the file format's
/// primary tag if it has none. An empty string or a `None` removes the item.
pub fn write_fields(path: &Path, fields: &Fields) -> Result<(), Failure> {
    edit_tag(path, |tag| {
        set_text(tag, ItemKey::TrackTitle, &fields.title);
        set_text(tag, ItemKey::TrackArtist, &fields.artist);
        set_text(tag, ItemKey::AlbumTitle, &fields.album);
        set_text(tag, ItemKey::AlbumArtist, &fields.album_artist);
        set_text(tag, ItemKey::Genre, &fields.genre);
        // Formats keep the year under different keys; the scanner reads `date` first, so
        // a stale `Year`/`RecordingDate` beside a fresh date would be harmless — but a
        // cleared year must clear all of them, or the old value comes back on the next scan.
        tag.remove_key(ItemKey::Year);
        tag.remove_key(ItemKey::RecordingDate);
        match fields.year.and_then(|y| u16::try_from(y).ok()) {
            Some(year) => tag.set_date(Timestamp {
                year,
                ..Timestamp::default()
            }),
            None => tag.remove_date(),
        }
        match fields.track_no {
            Some(n) => tag.set_track(n),
            None => tag.remove_track(),
        }
    })
}

/// Replace every embedded picture of `path` with `bytes` as the front cover.
///
/// Every picture goes, not just the front cover: the scanner (and the Music app) take the
/// *first* picture whatever its type, and real files tag their cover as `Other` often
/// enough that leaving those in would leave the old cover showing.
pub fn write_cover(path: &Path, bytes: &[u8], kind: ImageKind) -> Result<(), Failure> {
    let mime = match kind {
        ImageKind::Png => MimeType::Png,
        ImageKind::Jpeg => MimeType::Jpeg,
    };
    edit_tag(path, |tag| {
        while !tag.pictures().is_empty() {
            tag.remove_picture(0);
        }
        tag.push_picture(
            Picture::unchecked(bytes.to_vec())
                .mime_type(mime)
                .pic_type(PictureType::CoverFront)
                .build(),
        );
    })
}

/// Read `path`, hand its primary tag (made if missing) to `edit`, and save it back.
fn edit_tag(path: &Path, edit: impl FnOnce(&mut Tag)) -> Result<(), Failure> {
    let fail = |e: lofty::error::LoftyError| Failure::new("TAGS NOT WRITTEN", e.to_string());
    let mut tagged = lofty::read_from_path(path).map_err(fail)?;
    if tagged.primary_tag().is_none() {
        let kind = tagged.primary_tag_type();
        tagged.insert_tag(Tag::new(kind));
    }
    let Some(tag) = tagged.primary_tag_mut() else {
        return Err(Failure::new(
            "TAGS NOT WRITTEN",
            "the format has no tag to write",
        ));
    };
    edit(tag);
    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(fail)
}

/// Set a text item, or remove it when `value` is blank.
fn set_text(tag: &mut Tag, key: ItemKey, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        tag.remove_key(key);
    } else {
        tag.insert_text(key, value.to_string());
    }
}

/// After an edit: move the file to where its new fields say it belongs, and return where
/// it is now. A no-op when the name is unchanged.
///
/// The move claims the new name with `hard_link`, which fails rather than replaces, and
/// only then drops the old name. The folders it leaves behind are removed if that made
/// them empty — the album's and then the artist's, never the root.
pub fn refile(root: &Path, location: &Path, fields: &Fields) -> Result<PathBuf, Failure> {
    let wanted = target(root, fields, &ext_of(location));
    if wanted == location {
        return Ok(location.to_path_buf());
    }
    let fail = |e: io::Error| Failure::new("NOT MOVED", e.to_string());
    if let Some(dir) = wanted.parent() {
        fs::create_dir_all(dir).map_err(fail)?;
    }
    for n in 1..=MAX_SUFFIX {
        let dest = candidate(&wanted, n);
        // A case-only rename on a case-insensitive volume: the "taken" name is this file.
        if same_file(&dest, location) {
            fs::rename(location, &dest).map_err(fail)?;
            return Ok(dest);
        }
        match fs::hard_link(location, &dest) {
            Ok(()) => {
                fs::remove_file(location).map_err(fail)?;
                prune(root, location);
                return Ok(dest);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(fail(e)),
        }
    }
    Err(Failure::new(
        "NOT MOVED",
        format!("no free name for {}", wanted.display()),
    ))
}

/// True if both paths name the same existing file.
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Remove the album and artist folders `old` was in, if moving it out emptied them.
/// `remove_dir` refuses a folder with anything left in it, which is the whole check.
fn prune(root: &Path, old: &Path) {
    let mut dir = old.parent();
    for _ in 0..2 {
        let Some(d) = dir else { return };
        if d == root || !d.starts_with(root) || fs::remove_dir(d).is_err() {
            return;
        }
        dir = d.parent();
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{image_bytes, sample, write_tagged_wav, write_wav};
    use super::*;

    #[test]
    fn names_are_made_safe_for_one_path_component() {
        assert_eq!(sanitize("AC/DC", "x"), "AC_DC");
        assert_eq!(sanitize("Re: Stacks", "x"), "Re_ Stacks");
        assert_eq!(sanitize(r"Bad\Ass", "x"), "Bad_Ass");
        assert_eq!(
            sanitize(".38 Special", "x"),
            "_38 Special",
            "a dot would hide it"
        );
        assert_eq!(sanitize("..", "x"), "_.");
        assert_eq!(sanitize("  tab\there\n ", "x"), "tabhere");
        assert_eq!(sanitize("   ", "Unknown Album"), "Unknown Album");
        assert_eq!(sanitize("死のダンス", "x"), "死のダンス");
        // Long names are cut on a character boundary, never inside one.
        let long = "死".repeat(100); // 300 bytes
        let cut = sanitize(&long, "x");
        assert!(cut.len() <= MAX_NAME_BYTES && cut.chars().all(|c| c == '死'));
    }

    #[test]
    fn the_target_is_album_artist_album_and_numbered_title() {
        let root = Path::new("/lib");
        assert_eq!(
            target(root, &sample(), "WAV"),
            PathBuf::from("/lib/HOME/Odyssey/03 Resonance.wav")
        );
        let bare = Fields {
            title: "Intro".into(),
            ..Fields::default()
        };
        assert_eq!(
            target(root, &bare, "mp3"),
            PathBuf::from("/lib/Unknown Artist/Unknown Album/Intro.mp3")
        );
        let compilation = Fields {
            artist: "Guest".into(),
            album_artist: "Various Artists".into(),
            album: "Mix/Tape".into(),
            track_no: Some(12),
            ..bare
        };
        assert_eq!(
            target(root, &compilation, "m4a"),
            PathBuf::from("/lib/Various Artists/Mix_Tape/12 Intro.m4a")
        );
    }

    #[test]
    fn a_taken_name_is_never_overwritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("lib");
        let src = dir.path().join("in.wav");
        write_tagged_wav(&src, &sample());
        let first = place(&root, &src, &sample()).expect("first");
        let first_bytes = fs::read(&first).expect("read");
        let second = place(&root, &src, &sample()).expect("second");
        let third = place(&root, &src, &sample()).expect("third");
        assert_eq!(first, root.join("HOME/Odyssey/03 Resonance.wav"));
        assert_eq!(second, root.join("HOME/Odyssey/03 Resonance 2.wav"));
        assert_eq!(third, root.join("HOME/Odyssey/03 Resonance 3.wav"));
        assert_eq!(fs::read(&first).expect("read"), first_bytes);
        assert_eq!(
            fs::read(&second).expect("read"),
            fs::read(&src).expect("read")
        );
    }

    #[test]
    fn a_copy_is_dated_now_so_it_lands_in_recently_added() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("old.wav");
        write_wav(&src);
        let long_ago = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        File::options()
            .write(true)
            .open(&src)
            .expect("open")
            .set_modified(long_ago)
            .expect("set mtime");
        let copy = place(&dir.path().join("lib"), &src, &sample()).expect("copied");
        let modified = fs::metadata(&copy)
            .and_then(|m| m.modified())
            .expect("mtime");
        assert!(modified > long_ago + std::time::Duration::from_secs(86_400 * 365));
    }

    #[test]
    fn fields_can_be_cleared_as_well_as_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("x.wav");
        write_tagged_wav(&path, &sample());
        let cleared = Fields {
            title: "Resonance".into(),
            ..Fields::default()
        };
        write_fields(&path, &cleared).expect("written");
        let back = super::super::Fields::read(&path).expect("readable");
        assert_eq!(back.genre, "");
        assert_eq!(back.year, None);
        assert_eq!(back.track_no, None);
        assert_eq!(back.album, "");
    }

    #[test]
    fn a_new_cover_replaces_every_old_picture() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("x.wav");
        write_tagged_wav(&path, &sample());
        let old = image_bytes(4, image::ImageFormat::Png);
        let new = image_bytes(6, image::ImageFormat::Jpeg);
        write_cover(&path, &old, ImageKind::Png).expect("first");
        write_cover(&path, &new, ImageKind::Jpeg).expect("second");
        let tagged = lofty::read_from_path(&path).expect("read");
        let tag = tagged.primary_tag().expect("tag");
        assert_eq!(tag.pictures().len(), 1);
        assert_eq!(tag.pictures()[0].data(), new.as_slice());
        assert_eq!(tag.pictures()[0].pic_type(), PictureType::CoverFront);
        // …and the fields survived the picture write.
        assert_eq!(
            super::super::Fields::read(&path).expect("readable"),
            sample()
        );
    }

    #[test]
    fn refiling_moves_without_overwriting_and_prunes_empty_folders() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("lib");
        let src = dir.path().join("in.wav");
        write_tagged_wav(&src, &sample());
        let here = place(&root, &src, &sample()).expect("placed");
        // Somebody already has the name the edit asks for.
        let renamed = Fields {
            artist: "Home".into(),
            album_artist: "Home".into(),
            ..sample()
        };
        let occupied = target(&root, &renamed, "wav");
        let differs = !same_file(&occupied, &here);
        if differs {
            write_wav(&occupied);
        }
        let moved = refile(&root, &here, &renamed).expect("moved");
        if differs {
            // Case-sensitive volume: a different folder, and its occupant is kept.
            assert_eq!(moved, candidate(&occupied, 2));
            assert!(occupied.is_file());
            assert!(!root.join("HOME").exists(), "the emptied folders are gone");
        } else {
            // Case-insensitive volume (the macOS default): the same file, renamed in place.
            assert_eq!(moved, occupied);
        }
        assert!(moved.is_file());
        assert!(root.is_dir(), "the root itself is never pruned");
        // Refiling to the name it already has is a no-op.
        assert_eq!(refile(&root, &moved, &renamed).expect("no-op"), moved);
    }

    #[test]
    fn pruning_stops_at_a_folder_that_still_has_songs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("lib");
        let src = dir.path().join("in.wav");
        write_tagged_wav(&src, &sample());
        let a = place(&root, &src, &sample()).expect("a");
        let other = Fields {
            title: "Native".into(),
            track_no: Some(2),
            ..sample()
        };
        let b = place(&root, &src, &other).expect("b");
        let moved = refile(
            &root,
            &a,
            &Fields {
                album: "Elsewhere".into(),
                ..sample()
            },
        )
        .expect("moved");
        assert_eq!(moved, root.join("HOME/Elsewhere/03 Resonance.wav"));
        assert!(b.is_file() && root.join("HOME/Odyssey").is_dir());
    }
}
