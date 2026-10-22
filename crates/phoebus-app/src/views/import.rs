//! Settings' IMPORT section: a wide drop field on top, and under it the list of
//! everything dropped this session — cover, title, artist, album, and where each one
//! stands. It lives in Settings, between LIBRARY and THEME, because importing is the other
//! way of putting music into the library the LIBRARY section points at.
//!
//! ## What a drop does
//!
//! Audio files and folders dropped anywhere on the window are imported: the app hands them
//! over through [`State::drop_paths`], opens Settings, and asks this section to scroll
//! itself into view once ([`State::reveal`]). Folders are walked like the scanner walks the
//! library, on a side thread — a dropped folder can be huge or on a network share, and the
//! window must not freeze for it. Anything that is neither audio nor a cover candidate is
//! counted and named under the field instead of silently ignored. How a file gets into the
//! library — the Music app or a copy — is [`phoebus_core::import`]'s business; this
//! section only says which, in the line under its label.
//!
//! ## Editing
//!
//! An imported row opens an editor (`✎`): title, artist, album, album artist, genre, year,
//! track number, and the cover. `SAVE` sends the edit to the worker, which applies it the
//! way the song went in — so in Music-app mode the Music app writes the tags, and both
//! libraries agree.
//!
//! A new cover is an image file dropped on the window while an editor is open. Not on the
//! cover square specifically: during an OS drag, winit reports *which* files hover but not
//! *where* the pointer is, so a drop cannot be aimed at one square among many. With one
//! editor open there is exactly one place an image can mean, and the editor's cover square
//! lights up the moment an image hovers, which is what makes the rule obvious in use.
//!
//! ## Afterwards
//!
//! When the worker has run dry and at least one import or edit landed, [`State::poll`]
//! says so once and the app rescans, which is what makes the songs appear everywhere else.
//! The list itself lives for the session only and survives that rescan.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use egui::{Align2, Rect, Sense, Stroke, StrokeKind, TextureHandle, Ui, Vec2};
use phoebus_core::import::{self, Dropped, Edit, Failure, Fields, Kind, Mode, Outcome, Placed};
use phoebus_core::{AlbumKey, Library};

use crate::artwork::{self, Artwork};
use crate::importer::{CoverId, Job, Reply, RowId, SideJob, Worker};
use crate::theme;
use crate::views;
use crate::widgets;

/// How many skipped file names the notice spells out before it says `+N MORE`.
const NAMES_SHOWN: usize = 3;
/// How often a running import's spinner turns.
const SPIN_MS: u64 = 120;
/// Hovered paths looked at to label the drop field. A drag of a thousand files is labelled
/// by the first few just as well, and each look is a `stat` (once per drag, not per frame).
const HOVER_PROBE: usize = 64;
/// Narrowest the year and track inputs get (four digits and the margins); they grow to
/// half again that, sharing a third of the editor between them.
const NUMBER_FIELD_W: f32 = 72.0;
/// Gap between two editor inputs on one line.
const FIELD_GAP: f32 = 12.0;
/// Why `✎` is dead on a song the Music app did not copy.
const ORIGINAL_TIP: &str = "THE MUSIC APP KEPT YOUR ORIGINAL FILE — AN EDIT WOULD CHANGE IT, \
                            SO EDIT IT IN MUSIC IF YOU MEAN TO";

/// Where one dropped file stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Waiting for the worker.
    Queued,
    /// The worker is importing it now.
    Importing,
    /// In the library.
    Imported,
    /// It was inside the library root already; nothing was done.
    AlreadyInLibrary,
    /// The Music app did not answer in time and may have added it anyway. Counted as
    /// listed, so dropping it again does not add it twice.
    Uncertain(Failure),
    /// It could not be imported, and why.
    Failed(Failure),
}

/// How the last edit of a row went.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Note {
    /// All of it landed.
    Saved,
    /// Some of it landed (the rest is in the failure).
    Partly(Failure),
    /// None of it landed.
    Failed(Failure),
}

/// What the row shows as its cover.
enum Thumb {
    /// Nothing yet — the placeholder.
    None,
    /// An uploaded thumbnail, shared by every row with the same artwork.
    Image(TextureHandle),
    /// A library album's cover from the shared cache (the screenshot tour's sample rows).
    Album(AlbumKey),
}

/// One dropped file.
struct Row {
    id: RowId,
    /// The dropped file.
    src: PathBuf,
    /// The library root it was imported into — edits go there even if the root changes.
    root: PathBuf,
    /// Its fields as shown: once read, then as last saved.
    fields: Option<Fields>,
    status: Status,
    /// Where it went, once imported — what an edit is sent to.
    placed: Option<Placed>,
    thumb: Thumb,
    /// The Music app kept the file outside the root (its copy setting is off).
    outside: bool,
    /// The fields an edit in flight is writing.
    saving: Option<Fields>,
    /// How the last edit went.
    note: Option<Note>,
    /// A sample row of the screenshot tour: drawn like any other, never saved anywhere.
    sample: bool,
}

impl Row {
    /// Whether `✎` may open an editor on this row, or why not (the tooltip on a dead `✎`).
    /// `None` when there is nothing to edit yet, and no `✎` at all.
    fn editable(&self) -> Option<Result<(), &'static str>> {
        let imported = self.status == Status::Imported && (self.placed.is_some() || self.sample);
        if !imported {
            return None;
        }
        // The track points at the user's own file: editing it would rewrite the original.
        Some(if self.outside {
            Err(ORIGINAL_TIP)
        } else {
            Ok(())
        })
    }
}

/// The open row editor: the inputs as typed, and a cover waiting to be saved.
struct Editor {
    id: RowId,
    title: String,
    artist: String,
    album: String,
    album_artist: String,
    genre: String,
    year: String,
    track: String,
    /// A dropped image that passed the check, to be applied by `SAVE`.
    cover: Option<PathBuf>,
    /// Its thumbnail.
    preview: Option<TextureHandle>,
    /// An image is being checked.
    checking: bool,
    /// The last thing that went wrong, shown beside the buttons.
    error: Option<String>,
}

impl Editor {
    fn new(id: RowId, fields: &Fields) -> Editor {
        let number = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
        Editor {
            id,
            title: fields.title.clone(),
            artist: fields.artist.clone(),
            album: fields.album.clone(),
            album_artist: fields.album_artist.clone(),
            genre: fields.genre.clone(),
            year: number(fields.year),
            track: number(fields.track_no),
            cover: None,
            preview: None,
            checking: false,
            error: None,
        }
    }

    /// The inputs as [`Fields`], or what is wrong with them.
    fn fields(&self) -> Result<Fields, String> {
        let title = self.title.trim();
        if title.is_empty() {
            return Err("TITLE CANNOT BE EMPTY".to_string());
        }
        let number = |text: &str, max: u32, what: &str| -> Result<Option<u32>, String> {
            match text.trim() {
                "" => Ok(None),
                t => t
                    .parse::<u32>()
                    .ok()
                    .filter(|n| (1..=max).contains(n))
                    .map(Some)
                    .ok_or_else(|| format!("{what} MUST BE A NUMBER FROM 1 TO {max}")),
            }
        };
        Ok(Fields {
            title: title.to_string(),
            artist: self.artist.trim().to_string(),
            album: self.album.trim().to_string(),
            album_artist: self.album_artist.trim().to_string(),
            genre: self.genre.trim().to_string(),
            year: number(&self.year, 9999, "YEAR")?,
            track_no: number(&self.track, 999, "TRACK")?,
        })
    }
}

/// Everything the view remembers for the session.
pub struct State {
    /// The library root new imports go into. Set through [`State::set_root`].
    root: PathBuf,
    /// How `root` takes new songs — worked out once per root, never per frame.
    mode: Mode,
    /// A library scan is running. Written by the app every frame.
    pub scanning: bool,
    /// Newest drop first; each drop in its own order.
    rows: Vec<Row>,
    next_id: RowId,
    worker: Worker,
    /// Jobs and side jobs sent and not yet answered.
    jobs: usize,
    /// Drops still being walked.
    walking: usize,
    /// Something landed in the library since the last rescan request.
    dirty: bool,
    /// What the last drop could not use.
    notice: Option<String>,
    editor: Option<Editor>,
    /// Every source this session has a live row for — imported, in flight, already in the
    /// library, or of unknown fate. A set, so a re-drop of a thousand files is a thousand
    /// lookups, not a million comparisons. Failed sources leave it, so they can be retried.
    listed: HashSet<PathBuf>,
    /// One texture per distinct cover, shared by every row that shows it.
    textures: HashMap<CoverId, TextureHandle>,
    /// Scroll the section into view on the next frame it is drawn — set by a drop, so
    /// the user sees where the files went; consumed by that frame.
    reveal: bool,
    /// The hovered paths [`State::hover`] last looked at, and what it made of them.
    hover_key: Vec<Option<PathBuf>>,
    hover: Hover,
}

impl Default for State {
    fn default() -> State {
        State {
            root: PathBuf::new(),
            mode: Mode::Copy,
            scanning: false,
            rows: Vec::new(),
            next_id: 0,
            worker: Worker::default(),
            jobs: 0,
            walking: 0,
            dirty: false,
            notice: None,
            editor: None,
            listed: HashSet::new(),
            textures: HashMap::new(),
            reveal: false,
            hover_key: Vec::new(),
            hover: Hover::default(),
        }
    }
}

impl State {
    /// Point new imports at `root`, and work out how it takes them ([`Mode::for_root`]).
    /// Called by the app at start-up and whenever the library root changes.
    pub fn set_root(&mut self, root: PathBuf) {
        self.mode = Mode::for_root(&root);
        self.root = root;
    }

    /// Take a drop. The walk runs on a side thread ([`SideJob::Sort`]) — a dropped folder
    /// may be huge, or on a network share — and [`State::poll`] files what it finds.
    /// Returns `true` when there was anything to take, which is the app's cue to show this
    /// view.
    pub fn drop_paths(&mut self, ctx: &egui::Context, paths: &[PathBuf]) -> bool {
        if paths.is_empty() {
            return false;
        }
        let job = SideJob::Sort {
            paths: paths.to_vec(),
        };
        if self.worker.side(ctx, job) {
            self.jobs += 1;
            self.walking += 1;
        } else {
            self.notice = Some("COULD NOT READ THE DROP".to_string());
        }
        true
    }

    /// A walked drop: images go to the open editor, new audio files become rows, and what
    /// cannot be used is reported.
    fn sorted(&mut self, ctx: &egui::Context, dropped: Dropped) {
        self.walking = self.walking.saturating_sub(1);
        let mut notes: Vec<String> = Vec::new();

        if let Some(image) = dropped.images.first() {
            match &mut self.editor {
                Some(editor) => {
                    editor.error = None;
                    editor.checking = true;
                    let job = SideJob::Preview {
                        id: editor.id,
                        path: image.clone(),
                    };
                    if self.worker.side(ctx, job) {
                        self.jobs += 1;
                    } else {
                        editor.checking = false;
                        editor.error = Some("IMPORT WORKER NOT RUNNING".to_string());
                    }
                }
                None => notes
                    .push("TO CHANGE A COVER, OPEN A SONG WITH ✎ AND DROP THE IMAGE THEN".into()),
            }
        }

        let mut fresh: Vec<Row> = Vec::new();
        let mut repeated = 0;
        for src in dropped.audio {
            if !self.listed.insert(src.clone()) {
                repeated += 1;
                continue;
            }
            fresh.push(self.queue(ctx, src));
        }
        if repeated > 0 {
            notes.push(format!("{repeated} ALREADY IN THIS LIST"));
        }
        if !dropped.skipped.is_empty() {
            notes.push(skipped_note(&dropped.skipped));
        }
        // Newest drop on top, so what was just dropped is what is on screen.
        self.rows.splice(0..0, fresh);
        self.notice = (!notes.is_empty()).then(|| notes.join(theme::SEP));
    }

    /// A new row for `src`, its import already sent.
    fn queue(&mut self, ctx: &egui::Context, src: PathBuf) -> Row {
        let id = self.next_id;
        self.next_id += 1;
        let job = Job::Import {
            id,
            src: src.clone(),
            root: self.root.clone(),
            mode: self.mode,
        };
        let status = if self.worker.send(ctx, job) {
            self.jobs += 1;
            Status::Queued
        } else {
            self.listed.remove(&src);
            Status::Failed(Failure::new("IMPORT WORKER NOT RUNNING", ""))
        };
        Row {
            id,
            src,
            root: self.root.clone(),
            fields: None,
            status,
            placed: None,
            thumb: Thumb::None,
            outside: false,
            saving: None,
            note: None,
            sample: false,
        }
    }

    /// Collect the worker's replies. Returns `true` exactly once per settled batch that
    /// changed the library: the worker has nothing left and something landed — the moment
    /// to rescan. Call every frame (the app does, from `logic`).
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        let replies: Vec<Reply> = self.worker.replies().collect();
        for reply in replies {
            if reply.ends_a_job() {
                self.jobs = self.jobs.saturating_sub(1);
            }
            self.apply(ctx, reply);
        }
        if self.jobs == 0 && self.dirty {
            self.dirty = false;
            return true;
        }
        false
    }

    /// The shared texture for a cover, uploaded the first time it is seen.
    fn texture(&mut self, ctx: &egui::Context, thumb: crate::importer::Thumb) -> TextureHandle {
        self.textures
            .entry(thumb.cover)
            .or_insert_with(|| {
                ctx.load_texture(
                    format!("import-cover-{:016x}", thumb.cover),
                    thumb.image,
                    egui::TextureOptions::LINEAR,
                )
            })
            .clone()
    }

    fn apply(&mut self, ctx: &egui::Context, reply: Reply) {
        match reply {
            Reply::Sorted { dropped } => self.sorted(ctx, dropped),
            Reply::Read { id, fields, thumb } => {
                let texture = thumb.map(|t| self.texture(ctx, t));
                let Some(row) = self.rows.iter_mut().find(|r| r.id == id) else {
                    return;
                };
                match fields {
                    Ok(fields) => row.fields = Some(fields),
                    Err(e) => {
                        self.listed.remove(&row.src);
                        row.status = Status::Failed(e);
                    }
                }
                if let Some(texture) = texture {
                    row.thumb = Thumb::Image(texture);
                }
            }
            Reply::Started { id } => {
                if let Some(row) = self.rows.iter_mut().find(|r| r.id == id)
                    && row.status == Status::Queued
                {
                    row.status = Status::Importing;
                }
            }
            Reply::Imported { id, outcome } => {
                let Some(row) = self.rows.iter_mut().find(|r| r.id == id) else {
                    return;
                };
                match outcome {
                    Ok(Outcome::Imported(placed)) => {
                        row.outside = placed.outside(&row.root);
                        row.placed = Some(placed);
                        row.status = Status::Imported;
                        self.dirty = true;
                    }
                    Ok(Outcome::AlreadyInLibrary) => row.status = Status::AlreadyInLibrary,
                    Ok(Outcome::Uncertain(e)) => {
                        // It may well be in the library by now: rescan, and keep it listed.
                        row.status = Status::Uncertain(e);
                        self.dirty = true;
                    }
                    Err(e) => {
                        self.listed.remove(&row.src);
                        // A file that failed its read already says why; keep that reason.
                        if !matches!(row.status, Status::Failed(_)) {
                            row.status = Status::Failed(e);
                        }
                    }
                }
            }
            Reply::Edited { id, applied, thumb } => {
                let texture = thumb.map(|t| self.texture(ctx, t));
                let editing = self.editor.as_ref().is_some_and(|e| e.id == id);
                let Some(row) = self.rows.iter_mut().find(|r| r.id == id) else {
                    return;
                };
                let saved = row.saving.take();
                row.outside = applied.placed.outside(&row.root);
                row.placed = Some(applied.placed.clone());
                // Whatever landed is shown as landed, whatever else went wrong.
                if applied.fields
                    && let Some(fields) = saved
                {
                    row.fields = Some(fields);
                }
                if let Some(texture) = texture {
                    row.thumb = Thumb::Image(texture);
                }
                if applied.anything() {
                    self.dirty = true;
                }
                let note = match applied.failure {
                    None => Note::Saved,
                    Some(e) if applied.anything() => Note::Partly(e),
                    Some(e) => Note::Failed(e),
                };
                if editing && let Some(editor) = &mut self.editor {
                    if applied.cover {
                        editor.cover = None;
                        editor.preview = None;
                    }
                    match &note {
                        Note::Saved => self.editor = None,
                        Note::Partly(e) => {
                            editor.error = Some(format!("PARTLY SAVED — {}", e.reason))
                        }
                        Note::Failed(e) => editor.error = Some(e.reason.clone()),
                    }
                }
                row.note = Some(note);
            }
            Reply::Preview { id, path, image } => {
                let Some(editor) = self.editor.as_mut().filter(|e| e.id == id) else {
                    return;
                };
                editor.checking = false;
                match image {
                    Ok(thumb) => {
                        editor.preview = Some(ctx.load_texture(
                            format!("import-preview-{id}"),
                            thumb.image,
                            egui::TextureOptions::LINEAR,
                        ));
                        editor.cover = Some(path);
                    }
                    Err(e) => editor.error = Some(e.reason),
                }
            }
        }
    }

    /// Have the section scroll itself into view the next time it is drawn — once.
    pub fn reveal(&mut self) {
        self.reveal = true;
    }

    /// `Esc`: close the row editor, unsaved changes and all. `true` if one was open.
    pub fn close_editor(&mut self) -> bool {
        self.editor.take().is_some()
    }

    /// `✎`: open the editor on a row, or close it when it is already open there.
    fn toggle_editor(&mut self, id: RowId) {
        if self.editor.as_ref().is_some_and(|e| e.id == id) {
            self.editor = None;
            return;
        }
        let Some(row) = self.rows.iter().find(|r| r.id == id) else {
            return;
        };
        if row.editable() != Some(Ok(())) {
            return;
        }
        let fields = row.fields.clone().unwrap_or_default();
        self.editor = Some(Editor::new(id, &fields));
    }

    /// `SAVE`: check the inputs and send exactly what the user changed to the worker.
    fn save(&mut self, ctx: &egui::Context) {
        let State {
            rows,
            editor,
            worker,
            jobs,
            ..
        } = self;
        let Some(ed) = editor.as_mut() else { return };
        let fields = match ed.fields() {
            Ok(fields) => fields,
            Err(message) => {
                ed.error = Some(message);
                return;
            }
        };
        let Some(row) = rows.iter_mut().find(|r| r.id == ed.id) else {
            return;
        };
        if row.sample {
            ed.error = Some("A SAMPLE ROW — NOTHING TO SAVE TO".to_string());
            return;
        }
        if let Some(Err(why)) = row.editable() {
            ed.error = Some(why.to_string());
            return;
        }
        let Some(placed) = row.placed.clone() else {
            ed.error = Some("NOT IMPORTED YET".to_string());
            return;
        };
        // Against what the editor was showing — fallbacks included — so a field the user
        // left alone is never written, however it was filled in.
        let changed = row.fields.clone().unwrap_or_default().diff(&fields);
        if changed.is_empty() && ed.cover.is_none() {
            *editor = None;
            return;
        }
        let job = Job::Edit {
            id: row.id,
            placed,
            root: row.root.clone(),
            edit: Box::new(Edit {
                fields: fields.clone(),
                changed,
                cover: ed.cover.clone(),
            }),
        };
        if worker.send(ctx, job) {
            *jobs += 1;
            row.saving = Some(fields);
            row.note = None;
            ed.error = None;
        } else {
            ed.error = Some("IMPORT WORKER NOT RUNNING".to_string());
        }
    }

    /// `CLEAR`: forget the rows that are done. Rows still in flight stay, so no reply
    /// ever arrives for a row the user can no longer see. The sources stay *listed*: a
    /// cleared import dropped again is still a duplicate.
    fn clear_finished(&mut self) {
        self.rows.retain(|r| {
            r.saving.is_some() || matches!(r.status, Status::Queued | Status::Importing)
        });
        if let Some(editor) = &self.editor
            && !self.rows.iter().any(|r| r.id == editor.id)
        {
            self.editor = None;
        }
        let shown: HashSet<egui::TextureId> = self
            .rows
            .iter()
            .filter_map(|r| match &r.thumb {
                Thumb::Image(texture) => Some(texture.id()),
                _ => None,
            })
            .collect();
        self.textures
            .retain(|_, texture| shown.contains(&texture.id()));
        self.notice = None;
    }

    /// `(done, total)` for the list header.
    fn progress(&self) -> (usize, usize) {
        let done = self
            .rows
            .iter()
            .filter(|r| !matches!(r.status, Status::Queued | Status::Importing))
            .count();
        (done, self.rows.len())
    }

    /// True while anything on screen is moving (a spinner) and needs frames.
    fn busy(&self) -> bool {
        self.jobs > 0
            || self.rows.iter().any(|r| {
                r.saving.is_some() || matches!(r.status, Status::Queued | Status::Importing)
            })
    }

    /// What is hovering over the window, sorted by what letting go would do.
    ///
    /// Telling a folder from a file takes a `stat`, so it is done once when the hovered set
    /// changes — at the start of a drag — and remembered while the drag stays the same.
    fn hover(&mut self, ctx: &egui::Context) -> Hover {
        let key: Vec<Option<PathBuf>> = ctx.input(|i| {
            i.raw
                .hovered_files
                .iter()
                .take(HOVER_PROBE)
                .map(|f| f.path.clone())
                .collect()
        });
        if key != self.hover_key {
            self.hover = Hover::of(&key);
            self.hover_key = key;
        }
        self.hover
    }

    /// Fill the list with sample rows built from `library`, one of each status, with the
    /// editor open on the first — the `--shot` tour's `settings-import.png`. Nothing is sent to the
    /// worker, and `SAVE` refuses sample rows.
    pub fn seed_demo(&mut self, library: &Library) {
        let statuses = [
            (Status::Imported, Some(Note::Saved)),
            (Status::Imported, None),
            (
                Status::Failed(Failure::new(
                    "MUSIC APP REFUSED",
                    "the Music app added nothing (9001)",
                )),
                None,
            ),
            (Status::Importing, None),
            (Status::Queued, None),
            (Status::AlreadyInLibrary, None),
        ];
        // One song per album first, so the covers differ down the list; then whatever is
        // left, so a small library still shows every status.
        let mut picked: Vec<phoebus_core::TrackId> = library
            .albums()
            .iter()
            .filter_map(|key| library.album_tracks(key).first().copied())
            .take(statuses.len())
            .collect();
        for id in library.tracks_sorted() {
            if picked.len() >= statuses.len() {
                break;
            }
            if !picked.contains(id) {
                picked.push(*id);
            }
        }
        let tracks: Vec<_> = picked.iter().filter_map(|id| library.track(*id)).collect();
        self.rows.clear();
        for (track, (status, note)) in tracks.into_iter().zip(statuses) {
            let id = self.next_id;
            self.next_id += 1;
            self.rows.push(Row {
                id,
                src: track.abs_path(library.root()),
                root: library.root().to_path_buf(),
                fields: Some(Fields {
                    title: track.title.clone(),
                    artist: track.artist.clone(),
                    album: track.album.clone(),
                    album_artist: track.album_artist.clone(),
                    genre: track.genre.clone().unwrap_or_default(),
                    year: track.year,
                    track_no: track.track_no,
                }),
                status,
                placed: None,
                thumb: Thumb::Album(track.album_key.clone()),
                outside: false,
                saving: None,
                note,
                sample: true,
            });
        }
        self.notice = Some(skipped_note(&[
            PathBuf::from("cover.jpg"),
            PathBuf::from("notes.txt"),
        ]));
        self.editor = self
            .rows
            .first()
            .map(|row| Editor::new(row.id, row.fields.as_ref().unwrap_or(&Fields::default())));
    }
}

/// `SKIPPED 2 FILES THAT ARE NOT AUDIO: cover.jpg, notes.txt`.
fn skipped_note(skipped: &[PathBuf]) -> String {
    let names: Vec<String> = skipped
        .iter()
        .take(NAMES_SHOWN)
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect();
    let more = skipped.len().saturating_sub(NAMES_SHOWN);
    let plural = if skipped.len() == 1 { "" } else { "S" };
    let tail = if more > 0 {
        format!(", +{more} MORE")
    } else {
        String::new()
    };
    format!(
        "SKIPPED {} FILE{plural} THAT {} NOT AUDIO: {}{tail}",
        skipped.len(),
        if skipped.len() == 1 { "IS" } else { "ARE" },
        names.join(", ")
    )
}

/// What is being dragged over the window right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Hover {
    /// Files or folders that would be imported (an unknown path counts: better to light up
    /// for a drop that turns out empty than to stay dark for one that works).
    imports: usize,
    /// Images: a cover for the open editor.
    images: usize,
}

impl Hover {
    /// Sort hovered paths. An unknown path counts as an import: better to light up for a
    /// drop that turns out empty than to stay dark for one that works.
    fn of(paths: &[Option<PathBuf>]) -> Hover {
        let mut hover = Hover::default();
        for path in paths {
            match path.as_deref() {
                Some(path) if !path.is_dir() && import::kind_of(path) == Kind::Image => {
                    hover.images += 1;
                }
                Some(path) if !path.is_dir() && import::kind_of(path) == Kind::Other => {}
                _ => hover.imports += 1,
            }
        }
        hover
    }
}

/// Draw the IMPORT section into the Settings page (which does the scrolling).
pub fn section(ui: &mut Ui, art: &mut Artwork, st: &mut State) {
    let top = ui.cursor().min;
    views::section(ui, "IMPORT");
    if std::mem::take(&mut st.reveal) {
        // Instantly, not animated: the drop that asked for it is already done, and the
        // screenshot tour photographs the very next settled frames.
        let anchor = Rect::from_min_size(top, Vec2::new(ui.available_width(), 1.0));
        ui.scroll_to_rect_animation(
            anchor,
            Some(egui::Align::TOP),
            egui::style::ScrollAnimation::none(),
        );
    }
    let hover = st.hover(ui.ctx());
    views::subheading(ui, &mode_line(&st.root, st.mode, st.scanning));
    ui.add_space(theme::CARD_TEXT_GAP * 1.5);
    drop_zone(ui, st.editor.is_some(), hover);
    if st.walking > 0 {
        ui.add_space(theme::CARD_TEXT_GAP);
        views::subheading(ui, &widgets::spaced("LOOKING THROUGH THE DROP…"));
    } else if let Some(notice) = &st.notice {
        ui.add_space(theme::CARD_TEXT_GAP);
        views::subheading(ui, notice);
    }
    ui.add_space(theme::SECTION_GAP);
    list(ui, art, st, hover);
    if st.busy() {
        ui.ctx()
            .request_repaint_after(Duration::from_millis(SPIN_MS));
    }
}

/// The line under the section label: where new songs go, and through what.
fn mode_line(root: &Path, mode: Mode, scanning: bool) -> String {
    let shown = views::settings::display_path(root);
    let mut line = match mode {
        Mode::MusicApp => format!("ADDED THROUGH THE MUSIC APP, WHICH FILES THEM IN {shown}"),
        Mode::Copy => format!("COPIED INTO {shown} AS ALBUM ARTIST / ALBUM / NN TITLE"),
    };
    if scanning {
        line.push_str(theme::SEP);
        line.push_str("RESCANNING THE LIBRARY…");
    }
    line
}

/// The drop field. Idle it is a dashed outline that says what it takes; with files over
/// the window it fills with the accent wash, gets a solid accent edge, and says what
/// letting go will do.
fn drop_zone(ui: &mut Ui, editing: bool, hover: Hover) {
    let size = Vec2::new(ui.available_width(), theme::DROP_ZONE_H);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = theme::p();
    let painter = ui.painter();
    // Images alone with no editor open have nowhere to go: the field explains instead of
    // lighting up.
    let hot = hover.imports > 0;
    let lost_images = hover.images > 0 && hover.imports == 0 && !editing;

    if hot {
        painter.rect(
            rect,
            theme::corner(),
            p.selection_bg,
            Stroke::new(theme::ACTIVE_BAR_W, p.accent),
            StrokeKind::Inside,
        );
    } else {
        painter.rect_filled(rect, theme::corner(), p.bg1);
        let r = rect.shrink(0.5);
        let outline = [
            r.left_top(),
            r.right_top(),
            r.right_bottom(),
            r.left_bottom(),
            r.left_top(),
        ];
        painter.extend(egui::Shape::dashed_line(
            &outline,
            Stroke::new(theme::HAIRLINE_W, p.text_low),
            theme::DASH,
            theme::DASH,
        ));
    }

    let (title, detail, color) = if hot {
        let n = hover.imports;
        let what = if n == 1 { "ITEM" } else { "ITEMS" };
        (
            format!("RELEASE TO IMPORT {n} {what}"),
            "FOLDERS ARE SEARCHED FOR SONGS".to_string(),
            p.accent_text,
        )
    } else if lost_images {
        (
            "OPEN A SONG WITH ✎ TO DROP A COVER ON IT".to_string(),
            "AN IMAGE ALONE HAS NOTHING TO IMPORT".to_string(),
            p.text_mid,
        )
    } else {
        (
            "DROP SONGS OR ALBUMS HERE".to_string(),
            "FILES OR FOLDERS · M4A MP3 FLAC OGG WAV AIFF AAC".to_string(),
            p.text_hi,
        )
    };
    let icon_color = if hot { p.accent_text } else { p.text_mid };
    let center = rect.center();
    painter.text(
        center - Vec2::new(0.0, theme::ICON_DROP * 0.75),
        Align2::CENTER_CENTER,
        theme::GLYPH_IMPORT,
        theme::font_icon(theme::ICON_DROP),
        icon_color,
    );
    let max_w = rect.width() - 2.0 * theme::VIEW_PAD;
    let title = widgets::truncated(
        ui,
        &widgets::spaced(&title),
        theme::font_body(),
        color,
        max_w,
    );
    let title_pos = center + Vec2::new(0.0, theme::CARD_TEXT_GAP * 1.5);
    painter.galley(
        Align2::CENTER_CENTER
            .anchor_size(title_pos, title.size())
            .min,
        title,
        color,
    );
    let detail_color = if hot { p.text_mid } else { p.text_low };
    let detail = widgets::truncated(
        ui,
        &widgets::spaced(&detail),
        theme::font_small(),
        detail_color,
        max_w,
    );
    let detail_pos = title_pos + Vec2::new(0.0, theme::CARD_TEXT_GAP * 2.5);
    painter.galley(
        Align2::CENTER_CENTER
            .anchor_size(detail_pos, detail.size())
            .min,
        detail,
        detail_color,
    );
}

/// What one frame of the list asked for, applied once the rows are drawn.
enum Click {
    Edit(RowId),
    Save,
    Cancel,
    Clear,
}

/// The header, then every row, and the open editor under its row.
fn list(ui: &mut Ui, art: &mut Artwork, st: &mut State, hover: Hover) {
    let mut clicks: Vec<Click> = Vec::new();
    let (done, total) = st.progress();
    let label = if total == 0 {
        "IMPORTED THIS SESSION".to_string()
    } else {
        format!("IMPORTED THIS SESSION · {done} OF {total} DONE")
    };
    if list_header(ui, &label, done > 0) {
        clicks.push(Click::Clear);
    }
    if total == 0 {
        views::subheading(
            ui,
            &widgets::spaced("NOTHING YET — WHAT YOU DROP SHOWS UP HERE"),
        );
    }

    // No scroller of its own: the Settings page scrolls, and a scroller inside a
    // scroller would trap the wheel.
    let spin = spinner(ui);
    let State { rows, editor, .. } = &mut *st;
    for row in rows.iter() {
        let editing = editor.as_ref().is_some_and(|e| e.id == row.id);
        if row_ui(ui, art, row, editing, spin) {
            clicks.push(Click::Edit(row.id));
        }
        if editing && let Some(ed) = editor.as_mut() {
            let saving = row.saving.is_some();
            if let Some(click) = editor_ui(ui, art, row, ed, saving, hover) {
                clicks.push(click);
            }
        }
    }

    let ctx = ui.ctx().clone();
    for click in clicks {
        match click {
            Click::Edit(id) => st.toggle_editor(id),
            Click::Save => st.save(&ctx),
            Click::Cancel => st.editor = None,
            Click::Clear => st.clear_finished(),
        }
    }
}

/// The current frame of the text spinner the scanning screen also uses.
fn spinner(ui: &Ui) -> char {
    let frames = ['|', '/', '-', '\\'];
    let index = (ui.input(|i| i.time) * 1000.0 / SPIN_MS as f64) as usize % frames.len();
    frames[index]
}

/// `IMPORTED THIS SESSION · 3 OF 5 DONE` with a hairline under it, and `CLEAR` at the
/// right end when there is something finished to clear. Returns `true` on a `CLEAR` click.
fn list_header(ui: &mut Ui, label: &str, clearable: bool) -> bool {
    let height = ui.text_style_height(&egui::TextStyle::Small) + 4.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    let p = theme::p();
    let mut clicked = false;
    let mut label_w = rect.width();
    if clearable {
        let text = widgets::spaced("CLEAR");
        let galley = widgets::truncated(ui, &text, theme::font_small(), p.text_low, rect.width());
        let hit = Rect::from_min_max(
            egui::pos2(rect.right() - galley.size().x, rect.top()),
            rect.right_bottom(),
        )
        .expand2(Vec2::new(4.0, (theme::HIT_MIN - height).max(0.0) * 0.5));
        let response = ui
            .interact(hit, ui.id().with("import-clear"), Sense::click())
            .on_hover_text(
                egui::RichText::new("FORGET THE FINISHED ROWS")
                    .font(theme::font_small())
                    .color(p.text_mid),
            );
        let color = theme::hover_color(response.hovered(), p.text_low, p.text_hi);
        ui.painter().galley(
            egui::pos2(
                rect.right() - galley.size().x,
                rect.center().y - galley.size().y * 0.5,
            ),
            galley.clone(),
            color,
        );
        clicked = response.clicked();
        label_w -= galley.size().x + theme::CARD_TEXT_GAP;
    }
    widgets::text_left(
        ui,
        egui::pos2(rect.left(), rect.center().y),
        &widgets::spaced(label),
        theme::font_small(),
        p.text_low,
        label_w,
    );
    ui.add_space(4.0);
    let line = ui.available_rect_before_wrap();
    ui.painter().line_segment(
        [line.left_top(), egui::pos2(line.right(), line.top())],
        theme::hairline(),
    );
    ui.add_space(theme::CARD_TEXT_GAP * 0.5);
    clicked
}

/// How loud a status reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Low,
    Mid,
    Hi,
}

/// A row's status column: glyph, words, loudness, and the tooltip behind the words.
fn status_line(row: &Row, spin: char) -> (&'static str, String, Tone, Option<String>) {
    if row.saving.is_some() {
        return ("", format!("SAVING {spin}"), Tone::Mid, None);
    }
    match &row.note {
        Some(Note::Failed(e)) => {
            return (
                theme::GLYPH_WARNING,
                format!("NOT SAVED: {}", e.reason),
                Tone::Hi,
                Some(e.detail.clone()),
            );
        }
        Some(Note::Partly(e)) => {
            return (
                theme::GLYPH_WARNING,
                format!("PARTLY SAVED: {}", e.reason),
                Tone::Hi,
                Some(e.detail.clone()),
            );
        }
        _ => {}
    }
    match &row.status {
        Status::Queued => ("", "QUEUED".into(), Tone::Low, None),
        Status::Importing => ("", format!("IMPORTING {spin}"), Tone::Mid, None),
        Status::Imported if row.outside => (
            theme::GLYPH_WARNING,
            "IN MUSIC, NOT IN ITS MEDIA FOLDER".into(),
            Tone::Hi,
            Some(
                "The Music app kept the original file where it was. Turn on \
                 \"Copy files to Music Media folder when adding to Library\" in Music's \
                 settings for Phoebus to see new songs."
                    .into(),
            ),
        ),
        Status::Imported if row.note == Some(Note::Saved) => {
            (theme::GLYPH_CHECK, "SAVED".into(), Tone::Mid, None)
        }
        Status::Imported => (theme::GLYPH_CHECK, "IMPORTED".into(), Tone::Mid, None),
        Status::AlreadyInLibrary => ("", "ALREADY IN LIBRARY".into(), Tone::Low, None),
        Status::Uncertain(e) => (
            theme::GLYPH_WARNING,
            "UNKNOWN — CHECK MUSIC".into(),
            Tone::Hi,
            Some(format!(
                "The Music app did not answer in time, and may still have added the song. \
                 It will not be offered to Music again this session. ({})",
                e.detail
            )),
        ),
        Status::Failed(e) => (
            theme::GLYPH_WARNING,
            e.reason.clone(),
            Tone::Hi,
            (!e.detail.is_empty()).then(|| e.detail.clone()),
        ),
    }
}

/// One row: cover, title over artist, album, status, and `✎` once there is something to
/// edit. Returns `true` when `✎` was clicked.
fn row_ui(ui: &mut Ui, art: &mut Artwork, row: &Row, editing: bool, spin: char) -> bool {
    let (rect, response) = widgets::row(ui, theme::ROW_PLAYLIST, Sense::hover());
    widgets::row_background(ui, rect, response.hovered() || editing, false);
    let p = theme::p();

    let cover = Rect::from_min_size(
        egui::pos2(
            rect.left() + theme::CARD_TEXT_GAP * 0.5,
            rect.center().y - theme::PLAYLIST_ART * 0.5,
        ),
        Vec2::splat(theme::PLAYLIST_ART),
    );
    paint_thumb(ui, art, &row.thumb, cover);

    // Right to left: `✎`, status, album; the title column takes what is left.
    let edit_rect = Rect::from_center_size(
        egui::pos2(rect.right() - theme::HIT_MIN * 0.5, rect.center().y),
        Vec2::splat(theme::HIT_MIN),
    );
    let status_rect = Rect::from_min_max(
        egui::pos2(
            edit_rect.left() - theme::CARD_TEXT_GAP - theme::IMPORT_STATUS_W,
            rect.top(),
        ),
        egui::pos2(edit_rect.left() - theme::CARD_TEXT_GAP, rect.bottom()),
    );
    let album_x = status_rect.left() - theme::CARD_TEXT_GAP * 2.0 - theme::SONG_COL_W;
    let text_x = cover.right() + theme::CARD_TEXT_GAP * 1.5;

    let file_name = row
        .src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (title, artist, album) = match &row.fields {
        Some(f) => (f.title.as_str(), f.artist.as_str(), f.album.as_str()),
        None => (file_name.as_str(), "", ""),
    };
    let title_w = (album_x - text_x - theme::CARD_TEXT_GAP).max(0.0);
    widgets::text_left(
        ui,
        egui::pos2(text_x, rect.center().y - 8.0),
        title,
        theme::font_body(),
        p.text_hi,
        title_w,
    );
    widgets::text_left(
        ui,
        egui::pos2(text_x, rect.center().y + 9.0),
        if artist.is_empty() { "—" } else { artist },
        theme::font_small(),
        p.text_mid,
        title_w,
    );
    widgets::text_left(
        ui,
        egui::pos2(album_x, rect.center().y),
        if album.is_empty() { "—" } else { album },
        theme::font_small(),
        p.text_mid,
        theme::SONG_COL_W,
    );

    let (glyph, words, tone, tip) = status_line(row, spin);
    let color = match tone {
        Tone::Low => p.text_low,
        Tone::Mid => p.text_mid,
        Tone::Hi => p.text_hi,
    };
    let galley = widgets::icon_text(
        ui,
        glyph,
        theme::ICON_INLINE,
        &widgets::spaced(&words),
        theme::font_small(),
    );
    let clip = ui.painter().with_clip_rect(status_rect);
    clip.galley(
        egui::pos2(
            status_rect.left(),
            status_rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        color,
    );
    if let Some(tip) = tip {
        ui.interact(
            status_rect,
            ui.id().with(("import-status", row.id)),
            Sense::hover(),
        )
        .on_hover_text(
            egui::RichText::new(tip)
                .font(theme::font_small())
                .color(p.text_mid),
        );
    }

    let mut clicked = false;
    match row.editable() {
        None => {}
        Some(Ok(())) => {
            let response = ui.interact(
                edit_rect,
                ui.id().with(("import-edit", row.id)),
                Sense::click(),
            );
            let color = if editing {
                p.accent_text
            } else {
                theme::hover_color(response.hovered(), p.text_low, p.text_hi)
            };
            ui.painter().text(
                edit_rect.center(),
                Align2::CENTER_CENTER,
                theme::GLYPH_RENAME,
                theme::font_icon(theme::ICON_INLINE),
                color,
            );
            clicked = response
                .on_hover_text(
                    egui::RichText::new(if editing {
                        "CLOSE"
                    } else {
                        "EDIT FIELDS AND COVER"
                    })
                    .font(theme::font_small())
                    .color(p.text_mid),
                )
                .clicked();
        }
        // Dead, like a disabled button: drawn in the border colour, senses no click, and
        // says why.
        Some(Err(why)) => {
            ui.painter().text(
                edit_rect.center(),
                Align2::CENTER_CENTER,
                theme::GLYPH_RENAME,
                theme::font_icon(theme::ICON_INLINE),
                p.border,
            );
            ui.interact(
                edit_rect,
                ui.id().with(("import-edit", row.id)),
                Sense::hover(),
            )
            .on_hover_text(
                egui::RichText::new(why)
                    .font(theme::font_small())
                    .color(p.text_mid),
            );
        }
    }
    if !editing {
        widgets::hairline_bottom_from(ui, rect, text_x);
    }
    clicked
}

/// A cover square from whatever the row has.
fn paint_thumb(ui: &Ui, art: &mut Artwork, thumb: &Thumb, rect: Rect) {
    match thumb {
        Thumb::Image(texture) => {
            ui.painter().image(
                texture.id(),
                rect,
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        Thumb::Album(key) => artwork::paint_cover(ui, art, Some(key), rect),
        Thumb::None => artwork::paint_cover(ui, art, None, rect),
    }
}

/// The editor under its row: the cover (and drop target) on the left, the seven inputs
/// on the right, `SAVE` / `CANCEL` under them.
fn editor_ui(
    ui: &mut Ui,
    art: &mut Artwork,
    row: &Row,
    ed: &mut Editor,
    saving: bool,
    hover: Hover,
) -> Option<Click> {
    let p = theme::p();
    let mut click = None;
    egui::Frame::new()
        .fill(p.bg1)
        .stroke(theme::hairline())
        .corner_radius(theme::corner())
        .inner_margin(egui::Margin::same(theme::CARD_TEXT_GAP as i8 * 2))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                cover_slot(ui, art, row, ed, hover.images > 0);
                ui.add_space(theme::VIEW_PAD);
                ui.vertical(|ui| {
                    // Two lines, not three: the editor opens inside the list, and every
                    // line it takes is a row of the list pushed off screen.
                    let full = ui.available_width();
                    let third = (full - 2.0 * FIELD_GAP) / 3.0;
                    let number =
                        ((third - FIELD_GAP) * 0.5).clamp(NUMBER_FIELD_W, NUMBER_FIELD_W * 1.5);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = FIELD_GAP;
                        input(ui, "TITLE", &mut ed.title, third);
                        input(ui, "ARTIST", &mut ed.artist, third);
                        input(ui, "ALBUM", &mut ed.album, third);
                    });
                    ui.add_space(theme::CARD_TEXT_GAP);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = FIELD_GAP;
                        input(ui, "ALBUM ARTIST", &mut ed.album_artist, third);
                        input(ui, "GENRE", &mut ed.genre, third);
                        input(ui, "YEAR", &mut ed.year, number);
                        input(ui, "TRACK", &mut ed.track, number);
                    });
                    ui.add_space(theme::CARD_TEXT_GAP * 1.5);
                    ui.horizontal(|ui| {
                        if saving || ed.checking {
                            let why = if saving {
                                "SAVING"
                            } else {
                                "CHECKING THE IMAGE"
                            };
                            widgets::disabled_button(ui, "", "SAVE", why);
                        } else if widgets::primary_button(ui, theme::GLYPH_CHECK, "SAVE").clicked()
                        {
                            click = Some(Click::Save);
                        }
                        if widgets::secondary_button(ui, "", "CANCEL").clicked() {
                            click = Some(Click::Cancel);
                        }
                        ui.add_space(theme::CARD_TEXT_GAP);
                        let message = match (&ed.error, saving) {
                            (Some(error), _) => Some((theme::GLYPH_WARNING, error.as_str())),
                            (None, true) => Some(("", "SAVING…")),
                            (None, false) if ed.cover.is_some() => {
                                Some(("", "NEW COVER — SAVE TO APPLY IT"))
                            }
                            _ => None,
                        };
                        if let Some((glyph, text)) = message {
                            let galley = widgets::icon_text(
                                ui,
                                glyph,
                                theme::ICON_INLINE,
                                &widgets::spaced(text),
                                theme::font_small(),
                            );
                            let (rect, _) = ui.allocate_exact_size(
                                Vec2::new(galley.size().x, theme::HIT_MIN),
                                Sense::hover(),
                            );
                            let color = if ed.error.is_some() {
                                p.text_hi
                            } else {
                                p.text_mid
                            };
                            ui.painter().galley(
                                egui::pos2(rect.left(), rect.center().y - galley.size().y * 0.5),
                                galley,
                                color,
                            );
                        }
                    });
                });
            });
        });
    ui.add_space(theme::CARD_TEXT_GAP);
    click
}

/// The editor's cover square: the staged new cover, else the current one, else an empty
/// slot. While an image hovers over the window it is *the* drop target, and looks it.
fn cover_slot(ui: &mut Ui, art: &mut Artwork, row: &Row, ed: &Editor, hot: bool) {
    let p = theme::p();
    ui.vertical(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(theme::EDIT_COVER), Sense::hover());
        match &ed.preview {
            Some(texture) => paint_thumb(ui, art, &Thumb::Image(texture.clone()), rect),
            None if !matches!(row.thumb, Thumb::None) => paint_thumb(ui, art, &row.thumb, rect),
            None => {
                ui.painter().rect_filled(rect, theme::corner(), p.bg2);
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    theme::GLYPH_IMAGE,
                    theme::font_icon(theme::ICON_DROP),
                    p.text_low,
                );
            }
        }
        if hot {
            ui.painter().rect(
                rect,
                theme::corner(),
                p.selection_bg,
                Stroke::new(theme::ACTIVE_BAR_W, p.accent),
                StrokeKind::Inside,
            );
        } else {
            ui.painter()
                .rect_stroke(rect, theme::corner(), theme::hairline(), StrokeKind::Inside);
        }
        ui.add_space(4.0);
        // Short enough to sit under the square untruncated; the formats are in the
        // tooltip and in the error a wrong one gets.
        let (caption, color) = if hot {
            ("RELEASE TO USE", p.accent_text)
        } else if ed.checking {
            ("CHECKING…", p.text_mid)
        } else {
            ("DROP AN IMAGE", p.text_low)
        };
        let galley = widgets::truncated(
            ui,
            &widgets::spaced(caption),
            theme::font_micro(),
            color,
            theme::EDIT_COVER,
        );
        let (line, caption_response) = ui.allocate_exact_size(
            Vec2::new(theme::EDIT_COVER, galley.size().y),
            Sense::hover(),
        );
        caption_response.on_hover_text(
            egui::RichText::new("DROP A PNG OR JPEG ANYWHERE ON THE WINDOW WHILE THIS IS OPEN")
                .font(theme::font_small())
                .color(p.text_mid),
        );
        ui.painter().galley(
            Align2::CENTER_TOP
                .anchor_size(line.center_top(), galley.size())
                .min,
            galley,
            color,
        );
    });
}

/// A labelled single-line input `width` wide.
fn input(ui: &mut Ui, label: &str, text: &mut String, width: f32) {
    ui.vertical(|ui| {
        ui.set_width(width);
        widgets::micro(ui, label);
        ui.add_space(2.0);
        ui.add(
            egui::TextEdit::singleline(text)
                .font(egui::TextStyle::Body)
                .text_color(theme::p().text_hi)
                .desired_width(width)
                .margin(egui::Margin::symmetric(8, 6)),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::importer::tests::write_wav;
    use std::fs;

    fn editor() -> Editor {
        Editor::new(
            0,
            &Fields {
                title: "Resonance".into(),
                year: Some(2014),
                track_no: Some(3),
                ..Fields::default()
            },
        )
    }

    #[test]
    fn the_editor_round_trips_and_refuses_bad_input() {
        let mut ed = editor();
        let fields = ed.fields().expect("valid");
        assert_eq!(fields.year, Some(2014));
        assert_eq!(fields.track_no, Some(3));

        ed.year = " ".into();
        ed.track = String::new();
        let cleared = ed.fields().expect("blank numbers clear");
        assert_eq!((cleared.year, cleared.track_no), (None, None));

        ed.year = "MMXIV".into();
        assert_eq!(
            ed.fields().expect_err("roman"),
            "YEAR MUST BE A NUMBER FROM 1 TO 9999"
        );
        ed.year = "2014".into();
        ed.track = "0".into();
        assert!(ed.fields().expect_err("zero").starts_with("TRACK"));
        ed.track = "1".into();
        ed.title = "   ".into();
        assert_eq!(ed.fields().expect_err("empty"), "TITLE CANNOT BE EMPTY");
    }

    #[test]
    fn the_skipped_note_names_a_few_and_counts_the_rest() {
        let one = skipped_note(&[PathBuf::from("/a/notes.txt")]);
        assert_eq!(one, "SKIPPED 1 FILE THAT IS NOT AUDIO: notes.txt");
        let many: Vec<PathBuf> = (0..5)
            .map(|n| PathBuf::from(format!("/a/{n}.txt")))
            .collect();
        assert_eq!(
            skipped_note(&many),
            "SKIPPED 5 FILES THAT ARE NOT AUDIO: 0.txt, 1.txt, 2.txt, +2 MORE"
        );
    }

    #[test]
    fn the_mode_line_names_the_path_new_songs_take() {
        let copy = mode_line(Path::new("/srv/music"), Mode::Copy, false);
        assert_eq!(
            copy,
            "COPIED INTO /srv/music AS ALBUM ARTIST / ALBUM / NN TITLE"
        );
        let scanning = mode_line(Path::new("/srv/music"), Mode::Copy, true);
        assert!(scanning.ends_with("RESCANNING THE LIBRARY…"));
        let music = mode_line(Path::new("/m/Media.localized/Music"), Mode::MusicApp, false);
        assert!(music.starts_with("ADDED THROUGH THE MUSIC APP"), "{music}");
    }

    /// Drive the whole state machine against a temp root in copy mode, on the real
    /// threads: a drop of a folder plus a file already in the library, walked off the UI
    /// thread; the rows it makes, the notice, the rescan cue (exactly once), a re-drop
    /// that adds nothing, and an edit with a cover.
    #[test]
    fn a_drop_imports_reports_and_asks_for_one_rescan() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("lib");
        fs::create_dir_all(&root).expect("mkdir");
        let album = dir.path().join("drop/Album");
        write_wav(&album.join("01 One.wav"));
        write_wav(&album.join("02 Two.wav"));
        fs::write(album.join("notes.txt"), b"x").expect("write");
        let inside = root.join("Old/Song/01 Old.wav");
        write_wav(&inside);

        let ctx = egui::Context::default();
        let mut st = State::default();
        st.set_root(root.clone());
        assert!(st.drop_paths(&ctx, &[album.clone(), inside.clone()]));
        assert!(
            st.rows.is_empty(),
            "the walk is not done on the calling thread"
        );
        assert_eq!(st.walking, 1);

        let rescans = settle(&ctx, &mut st);
        assert_eq!(rescans, 1, "one rescan for the whole batch");
        assert_eq!(st.walking, 0);
        assert_eq!(st.rows.len(), 3);
        assert_eq!(
            st.notice.as_deref(),
            Some("SKIPPED 1 FILE THAT IS NOT AUDIO: notes.txt")
        );
        let statuses: Vec<&Status> = st.rows.iter().map(|r| &r.status).collect();
        assert_eq!(
            statuses,
            [
                &Status::Imported,
                &Status::Imported,
                &Status::AlreadyInLibrary
            ]
        );
        assert!(
            root.join("Unknown Artist/Unknown Album/01 One.wav")
                .is_file()
        );

        // The same folder again: nothing new, and the list says why.
        assert!(st.drop_paths(&ctx, std::slice::from_ref(&album)));
        assert_eq!(settle(&ctx, &mut st), 0, "nothing landed, no rescan");
        assert_eq!(st.rows.len(), 3);
        assert!(
            st.notice
                .as_deref()
                .is_some_and(|n| n.starts_with("2 ALREADY IN THIS LIST"))
        );

        // Edit the first imported row: two fields, and a cover dropped on the editor.
        let id = st.rows[0].id;
        st.toggle_editor(id);
        let cover = dir.path().join("cover.png");
        fs::write(&cover, png()).expect("write");
        assert!(st.drop_paths(&ctx, std::slice::from_ref(&cover)));
        settle(&ctx, &mut st);
        assert_eq!(
            st.editor.as_ref().and_then(|e| e.cover.clone()),
            Some(cover)
        );
        if let Some(ed) = &mut st.editor {
            ed.artist = "HOME".into();
            ed.album = "Odyssey".into();
        }
        st.save(&ctx);
        assert_eq!(settle(&ctx, &mut st), 1, "an edit is a library change too");
        assert!(st.editor.is_none(), "a saved editor closes");
        let row = &st.rows[0];
        assert_eq!(row.note, Some(Note::Saved));
        assert!(matches!(row.thumb, Thumb::Image(_)));
        let moved = row
            .placed
            .as_ref()
            .and_then(|p| p.location.clone())
            .expect("location");
        // The album artist was a fallback the user never touched, so it was not written:
        // the file is filed under the artist it now has, by the scanner's own rule.
        assert_eq!(moved, root.join("HOME/Odyssey/01 One.wav"));
        assert!(import::embedded_picture(&moved).is_some());

        // CLEAR forgets what is finished — and its textures — but not what was imported:
        // dropping the folder again is still a duplicate.
        st.clear_finished();
        assert!(st.rows.is_empty() && st.notice.is_none() && st.textures.is_empty());
        st.drop_paths(&ctx, std::slice::from_ref(&album));
        settle(&ctx, &mut st);
        assert!(st.rows.is_empty());
    }

    #[test]
    fn an_image_with_no_editor_open_is_explained_not_lost() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cover = dir.path().join("cover.png");
        fs::write(&cover, png()).expect("write");
        let ctx = egui::Context::default();
        let mut st = State::default();
        assert!(st.drop_paths(&ctx, &[cover]));
        settle(&ctx, &mut st);
        assert!(st.rows.is_empty());
        assert!(
            st.notice
                .as_deref()
                .is_some_and(|n| n.contains("OPEN A SONG"))
        );
    }

    /// A bare imported row, for driving replies by hand.
    fn imported_row(st: &mut State, src: &Path, placed: Option<Placed>) -> RowId {
        let id = st.next_id;
        st.next_id += 1;
        st.listed.insert(src.to_path_buf());
        st.rows.push(Row {
            id,
            src: src.to_path_buf(),
            root: PathBuf::from("/lib"),
            fields: Some(Fields {
                title: "One".into(),
                artist: "Guest".into(),
                album_artist: "Guest".into(),
                ..Fields::default()
            }),
            status: Status::Queued,
            placed,
            thumb: Thumb::None,
            outside: false,
            saving: None,
            note: None,
            sample: false,
        });
        id
    }

    /// A timed-out add may have landed in Music: the row says so, the library rescans,
    /// and dropping the file again does not offer it to Music a second time. A failure,
    /// by contrast, can be retried.
    #[test]
    fn an_add_of_unknown_fate_is_never_offered_twice() {
        let ctx = egui::Context::default();
        let mut st = State::default();
        let src = PathBuf::from("/nonexistent/phoebus/song.m4a");
        let id = imported_row(&mut st, &src, None);
        st.apply(
            &ctx,
            Reply::Imported {
                id,
                outcome: Ok(Outcome::Uncertain(Failure::new(
                    music_did_not_answer(),
                    "(-1712)",
                ))),
            },
        );
        assert!(matches!(st.rows[0].status, Status::Uncertain(_)));
        assert_eq!(status_line(&st.rows[0], '|').1, "UNKNOWN — CHECK MUSIC");
        assert!(st.dirty, "it may be in the library: rescan");
        st.sorted(
            &ctx,
            Dropped {
                audio: vec![src.clone()],
                ..Dropped::default()
            },
        );
        assert_eq!(st.rows.len(), 1, "not queued again");
        assert_eq!(st.notice.as_deref(), Some("1 ALREADY IN THIS LIST"));

        // A plain failure leaves the list, so the same file can be tried again.
        let other = PathBuf::from("/nonexistent/phoebus/other.m4a");
        let id = imported_row(&mut st, &other, None);
        st.apply(
            &ctx,
            Reply::Imported {
                id,
                outcome: Err(Failure::new("MUSIC APP REFUSED", "")),
            },
        );
        assert!(!st.listed.contains(&other));
    }

    fn music_did_not_answer() -> &'static str {
        import::music_app::DID_NOT_ANSWER
    }

    /// A song the Music app did not copy points at the user's own file: `✎` is dead and
    /// says why, the editor will not open, and `SAVE` would refuse anyway.
    #[test]
    fn a_song_music_did_not_copy_cannot_be_edited() {
        let mut st = State::default();
        let placed = Placed {
            mode: Mode::MusicApp,
            location: Some(PathBuf::from("/Users/x/Downloads/song.m4a")),
            music_id: Some("0123456789ABCDEF".into()),
        };
        let id = imported_row(
            &mut st,
            Path::new("/Users/x/Downloads/song.m4a"),
            Some(placed),
        );
        st.rows[0].status = Status::Imported;
        st.rows[0].outside = true;
        assert_eq!(st.rows[0].editable(), Some(Err(ORIGINAL_TIP)));
        st.toggle_editor(id);
        assert!(st.editor.is_none());
        // Even an editor opened before the fact cannot save.
        st.editor = Some(Editor::new(id, &Fields::default()));
        if let Some(ed) = &mut st.editor {
            ed.title = "Changed".into();
        }
        st.save(&egui::Context::default());
        assert_eq!(st.jobs, 0);
        assert_eq!(
            st.editor.as_ref().and_then(|e| e.error.as_deref()),
            Some(ORIGINAL_TIP)
        );
    }

    /// Fields written, cover refused: the row keeps the new fields, says it is only partly
    /// saved, and still asks for the rescan.
    #[test]
    fn a_partial_edit_keeps_what_landed_and_rescans() {
        let ctx = egui::Context::default();
        let mut st = State::default();
        let placed = Placed {
            mode: Mode::Copy,
            location: Some(PathBuf::from("/lib/Guest/One.wav")),
            music_id: None,
        };
        let id = imported_row(&mut st, Path::new("/in/one.wav"), Some(placed.clone()));
        st.rows[0].status = Status::Imported;
        st.toggle_editor(id);
        let new = Fields {
            title: "Uno".into(),
            ..st.rows[0].fields.clone().unwrap_or_default()
        };
        st.rows[0].saving = Some(new.clone());
        st.apply(
            &ctx,
            Reply::Edited {
                id,
                applied: import::Applied {
                    placed,
                    fields: true,
                    cover: false,
                    failure: Some(Failure::new("NOT A PNG OR JPEG", "x.gif")),
                },
                thumb: None,
            },
        );
        let row = &st.rows[0];
        assert_eq!(row.fields.as_ref(), Some(&new));
        assert_eq!(
            row.note,
            Some(Note::Partly(Failure::new("NOT A PNG OR JPEG", "x.gif")))
        );
        assert_eq!(status_line(row, '|').1, "PARTLY SAVED: NOT A PNG OR JPEG");
        assert!(st.dirty);
        assert_eq!(
            st.editor.as_ref().and_then(|e| e.error.as_deref()),
            Some("PARTLY SAVED — NOT A PNG OR JPEG"),
            "the editor stays open on what is left"
        );
    }

    /// An album's tracks share their artwork: one texture for all of them.
    #[test]
    fn identical_covers_share_one_texture() {
        let ctx = egui::Context::default();
        let mut st = State::default();
        let image = crate::importer::thumbnail(&png()).expect("decodes");
        let thumb = crate::importer::Thumb { cover: 7, image };
        let a = imported_row(&mut st, Path::new("/in/a.wav"), None);
        let b = imported_row(&mut st, Path::new("/in/b.wav"), None);
        for id in [a, b] {
            st.apply(
                &ctx,
                Reply::Read {
                    id,
                    fields: Ok(Fields::default()),
                    thumb: Some(thumb.clone()),
                },
            );
        }
        assert_eq!(st.textures.len(), 1);
        let ids: Vec<_> = st
            .rows
            .iter()
            .filter_map(|r| match &r.thumb {
                Thumb::Image(t) => Some(t.id()),
                _ => None,
            })
            .collect();
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], ids[1]);
    }

    #[test]
    fn sample_rows_are_never_saved() {
        let lib = sample_library();
        let mut st = State::default();
        st.seed_demo(&lib);
        assert_eq!(st.rows.len(), 2);
        assert!(st.editor.is_some(), "the tour shows the editor open");
        if let Some(ed) = &mut st.editor {
            ed.title = "Changed".into();
        }
        st.save(&egui::Context::default());
        assert_eq!(st.jobs, 0);
        assert!(
            st.editor
                .as_ref()
                .and_then(|e| e.error.as_deref())
                .is_some_and(|e| e.contains("SAMPLE"))
        );
    }

    #[test]
    fn statuses_read_as_designed() {
        let lib = sample_library();
        let mut st = State::default();
        st.seed_demo(&lib);
        let row = &mut st.rows[0];
        assert_eq!(status_line(row, '|').1, "SAVED");
        row.note = None;
        assert_eq!(status_line(row, '|').1, "IMPORTED");
        row.outside = true;
        assert_eq!(status_line(row, '|').0, theme::GLYPH_WARNING);
        row.status = Status::Failed(Failure::new("MUSIC APP REFUSED", "detail"));
        let (glyph, words, tone, tip) = status_line(row, '|');
        assert_eq!(
            (glyph, words.as_str(), tone, tip.as_deref()),
            (
                theme::GLYPH_WARNING,
                "MUSIC APP REFUSED",
                Tone::Hi,
                Some("detail")
            )
        );
        row.saving = Some(Fields::default());
        assert_eq!(status_line(row, '/').1, "SAVING /");
    }

    /// Hovered files light the field, or the editor's cover, by what they are — and the
    /// folder check behind that runs once per drag, not once per frame.
    #[test]
    fn hovering_is_sorted_by_what_would_happen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let folder = dir.path().join("Album.Vol.2");
        fs::create_dir_all(&folder).expect("mkdir");
        let paths = |ps: &[&Path]| -> Vec<Option<PathBuf>> {
            ps.iter().map(|p| Some(p.to_path_buf())).collect()
        };
        assert_eq!(
            Hover::of(&paths(&[
                &folder,
                Path::new("/x/a.mp3"),
                Path::new("/x/c.jpg")
            ])),
            Hover {
                imports: 2,
                images: 1
            }
        );
        assert_eq!(
            Hover::of(&paths(&[Path::new("/x/readme.md")])),
            Hover::default()
        );

        // Through the state: the answer is cached against the hovered set.
        let ctx = egui::Context::default();
        let mut st = State::default();
        let input = egui::RawInput {
            hovered_files: vec![egui::HoveredFile {
                path: Some(folder.clone()),
                ..Default::default()
            }],
            ..Default::default()
        };
        for _ in 0..2 {
            let mut out = ctx.run_ui(input.clone(), |ui| {
                assert_eq!(st.hover(ui.ctx()).imports, 1);
            });
            // API-FACTS §3.7: dropping unapplied texture deltas panics.
            out.textures_delta.clear();
        }
        assert_eq!(st.hover_key, paths(&[&folder]));
    }

    /// Poll until every job has answered; return how many times `poll` asked for a rescan.
    fn settle(ctx: &egui::Context, st: &mut State) -> usize {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut rescans = 0;
        loop {
            if st.poll(ctx) {
                rescans += 1;
            }
            if st.jobs == 0 {
                return rescans;
            }
            assert!(std::time::Instant::now() < deadline, "the worker hung");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn png() -> Vec<u8> {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::new(8, 8));
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("encode");
        out
    }

    fn sample_library() -> Library {
        let mut a = phoebus_core::Track::new("HOME/Odyssey/01 Intro.m4a");
        a.refresh_key();
        let mut b = phoebus_core::Track::new("Woodkid/S16/01 Goliath.m4a");
        b.refresh_key();
        Library::build("/lib", vec![a, b])
    }
}
