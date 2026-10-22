//! The import worker: one background thread that does everything an import or an edit
//! blocks on — reading tags, decoding cover thumbnails, copying files, and waiting on the
//! Music app — so no frame ever does.
//!
//! The thread is started on the first job, not with the app: most runs never import
//! anything, and the headless render tests build a [`crate::views::ViewState`] by the dozen.
//!
//! Jobs run one at a time, in order. That is not only simple, it is required: the Music app
//! handles one `add` at a time anyway, and two edits of the same song must land in the order
//! they were saved. A drop of a whole album is still quick to *show*, because the worker
//! takes every job that is already waiting as one batch and reads all of their tags before
//! it imports the first file ([`work`]) — the list fills in with titles and covers at once,
//! then the statuses tick over one by one.
//!
//! Every reply wakes the UI (`request_repaint`), which is what keeps the list moving even
//! while nothing else is animating, and what lets a hidden window's `logic` pass collect a
//! finished import and start the rescan.

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use phoebus_core::import::{self, Failure, Fields, Mode, Outcome, Placed};

/// Longest edge of a decoded thumbnail. Twice the editor's cover, so it stays sharp there
/// and on a 2× display in the row.
pub const THUMB_EDGE: u32 = 240;

/// Identifies one row of the import list for the life of the session.
pub type RowId = u64;

/// Something for the worker to do.
pub enum Job {
    /// Read `src`, then import it into `root` by `mode`.
    Import {
        /// The row this belongs to.
        id: RowId,
        /// The dropped file.
        src: PathBuf,
        /// The library root at the time of the drop.
        root: PathBuf,
        /// How the root takes new songs.
        mode: Mode,
    },
    /// Apply new fields and/or a new cover to a song imported earlier.
    Edit {
        /// The row this belongs to.
        id: RowId,
        /// Where the song is and how to reach it.
        placed: Placed,
        /// The library root, for re-filing a copy.
        root: PathBuf,
        /// New fields, if any changed.
        fields: Option<Fields>,
        /// A new cover image, if one was dropped.
        cover: Option<PathBuf>,
    },
    /// Check and decode an image the user dropped on the editor, for its preview.
    Preview {
        /// The row whose editor it was dropped on.
        id: RowId,
        /// The image file.
        path: PathBuf,
    },
}

/// What the worker reports back, in the order things happen.
pub enum Reply {
    /// The tags of a queued file (or why it is not audio), and its embedded cover.
    Read {
        /// The row.
        id: RowId,
        /// The file's fields, or the reason it cannot be imported.
        fields: Result<Fields, Failure>,
        /// The embedded cover, decoded and shrunk.
        thumb: Option<Arc<egui::ColorImage>>,
    },
    /// The worker has started on this row's job.
    Started {
        /// The row.
        id: RowId,
    },
    /// An import finished.
    Imported {
        /// The row.
        id: RowId,
        /// What happened.
        outcome: Result<Outcome, Failure>,
    },
    /// An edit finished.
    Edited {
        /// The row.
        id: RowId,
        /// Where the song is now, or why the edit failed.
        result: Result<Placed, Failure>,
        /// The new cover's thumbnail, when the edit set one.
        thumb: Option<Arc<egui::ColorImage>>,
    },
    /// A dropped cover image, checked and decoded — or why it cannot be a cover.
    Preview {
        /// The row.
        id: RowId,
        /// The image file.
        path: PathBuf,
        /// The thumbnail, or the reason.
        image: Result<Arc<egui::ColorImage>, Failure>,
    },
}

impl Reply {
    /// True for the replies that end a job (and so balance one [`Worker::send`]).
    pub fn ends_a_job(&self) -> bool {
        matches!(
            self,
            Reply::Imported { .. } | Reply::Edited { .. } | Reply::Preview { .. }
        )
    }
}

/// The UI's handle on the worker thread.
pub struct Worker {
    jobs: Option<Sender<Job>>,
    replies_tx: Sender<Reply>,
    replies: Receiver<Reply>,
}

impl Default for Worker {
    fn default() -> Worker {
        let (replies_tx, replies) = crossbeam_channel::unbounded();
        Worker {
            jobs: None,
            replies_tx,
            replies,
        }
    }
}

impl Worker {
    /// Queue a job, starting the thread first if this is the first one.
    ///
    /// Returns `false` when the thread could not be started; the caller then reports the
    /// job as failed instead of waiting on it forever.
    pub fn send(&mut self, ctx: &egui::Context, job: Job) -> bool {
        if self.jobs.is_none() {
            let (tx, rx) = crossbeam_channel::unbounded::<Job>();
            let replies = self.replies_tx.clone();
            let ctx = ctx.clone();
            let spawned = std::thread::Builder::new()
                .name("phoebus-import".to_string())
                .spawn(move || work(&rx, &replies, &ctx));
            match spawned {
                Ok(_) => self.jobs = Some(tx),
                Err(e) => {
                    log::error!("import: could not start the worker thread: {e}");
                    return false;
                }
            }
        }
        self.jobs.as_ref().is_some_and(|tx| tx.send(job).is_ok())
    }

    /// Every reply that has arrived since the last call.
    pub fn replies(&self) -> impl Iterator<Item = Reply> + '_ {
        self.replies.try_iter()
    }
}

/// The thread body: take whatever is waiting as one batch, read every import's tags first,
/// then run the batch in order.
fn work(jobs: &Receiver<Job>, replies: &Sender<Reply>, ctx: &egui::Context) {
    let send = |reply: Reply| {
        let ok = replies.send(reply).is_ok();
        ctx.request_repaint();
        ok
    };
    while let Ok(first) = jobs.recv() {
        let mut batch: Vec<Job> = vec![first];
        batch.extend(jobs.try_iter());

        // Pass 1: tags and covers for every file in the batch, so the whole drop shows up
        // with titles before the first (possibly slow) import begins.
        let mut read: Vec<Option<Fields>> = Vec::with_capacity(batch.len());
        for job in &batch {
            let Job::Import { id, src, .. } = job else {
                read.push(None);
                continue;
            };
            let fields = Fields::read(src);
            let thumb = fields
                .is_ok()
                .then(|| import::embedded_picture(src))
                .flatten()
                .and_then(|bytes| thumbnail(&bytes));
            read.push(fields.as_ref().ok().cloned());
            if !send(Reply::Read {
                id: *id,
                fields,
                thumb,
            }) {
                return;
            }
        }

        // Pass 2: the work itself.
        for (job, fields) in batch.into_iter().zip(read) {
            let reply = match job {
                Job::Import {
                    id,
                    src,
                    root,
                    mode,
                } => {
                    let Some(fields) = fields else {
                        // Not audio: `Read` already said so. The job still has to end.
                        send(Reply::Imported {
                            id,
                            outcome: Err(Failure::new("NOT READABLE AS AUDIO", "")),
                        });
                        continue;
                    };
                    send(Reply::Started { id });
                    let outcome = import::import(mode, &root, &src, &fields);
                    match &outcome {
                        Ok(Outcome::Imported(placed)) => log::info!(
                            "import: {} -> {:?}",
                            src.display(),
                            placed.location.as_deref()
                        ),
                        Ok(Outcome::AlreadyInLibrary) => {
                            log::info!("import: {} is already in the library", src.display());
                        }
                        Err(e) => log::warn!("import: {}: {e}", src.display()),
                    }
                    Reply::Imported { id, outcome }
                }
                Job::Edit {
                    id,
                    placed,
                    root,
                    fields,
                    cover,
                } => {
                    send(Reply::Started { id });
                    let result = import::apply(&placed, &root, fields.as_ref(), cover.as_deref());
                    if let Err(e) = &result {
                        log::warn!("import: edit failed: {e}");
                    }
                    let thumb = match (&result, &cover) {
                        (Ok(_), Some(path)) => std::fs::read(path).ok().and_then(|b| thumbnail(&b)),
                        _ => None,
                    };
                    Reply::Edited { id, result, thumb }
                }
                Job::Preview { id, path } => {
                    let image = import::read_cover(&path).and_then(|(bytes, _)| {
                        thumbnail(&bytes).ok_or_else(|| Failure::new("IMAGE DOES NOT DECODE", ""))
                    });
                    Reply::Preview { id, path, image }
                }
            };
            if !send(reply) {
                return;
            }
        }
    }
}

/// Decode an image and shrink it to at most [`THUMB_EDGE`] on its longest side.
pub fn thumbnail(bytes: &[u8]) -> Option<Arc<egui::ColorImage>> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width() > THUMB_EDGE || img.height() > THUMB_EDGE {
        img.thumbnail(THUMB_EDGE, THUMB_EDGE)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    if size[0] == 0 || size[1] == 0 {
        return None;
    }
    Some(Arc::new(egui::ColorImage::from_rgba_unmultiplied(
        size,
        rgba.as_raw(),
    )))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;
    use std::time::Duration;

    /// A tenth of a second of 8 kHz mono silence as a WAV — the scanner's fixture, which
    /// `lofty` reads as audio with no tags. Shared with the Import view's tests.
    pub(crate) fn write_wav(path: &std::path::Path) {
        let data_len: u32 = 1600;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&8000u32.to_le_bytes());
        b.extend_from_slice(&16000u32.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        b.resize(44 + data_len as usize, 0);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, b).expect("write wav");
    }

    fn png(edge: u32) -> Vec<u8> {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::new(edge, edge));
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("encode");
        out
    }

    #[test]
    fn thumbnails_are_capped_and_garbage_is_none() {
        let big = thumbnail(&png(1000)).expect("decodes");
        assert_eq!(big.size, [THUMB_EDGE as usize; 2]);
        let small = thumbnail(&png(10)).expect("decodes");
        assert_eq!(small.size, [10, 10], "never blown up");
        assert!(thumbnail(b"not an image").is_none());
    }

    /// Wait for replies until `n` jobs have ended (or give up after a few seconds).
    fn collect(worker: &Worker, n: usize) -> Vec<Reply> {
        let mut out = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while out.iter().filter(|r: &&Reply| r.ends_a_job()).count() < n {
            assert!(std::time::Instant::now() < deadline, "the worker hung");
            out.extend(worker.replies());
            std::thread::sleep(Duration::from_millis(5));
        }
        out
    }

    /// The whole round trip on a real thread, in copy mode against a temp root: a batch is
    /// read before it is imported, a non-audio file ends its job with a failure instead of
    /// stalling the list, and an import lands where the core says it does.
    #[test]
    fn a_batch_is_read_first_then_imported_in_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("lib");
        std::fs::create_dir_all(&root).expect("mkdir");
        let good = dir.path().join("in/01 One.wav");
        write_wav(&good);
        let bad = dir.path().join("in/fake.mp3");
        std::fs::write(&bad, b"nope").expect("write");

        let ctx = egui::Context::default();
        let mut worker = Worker::default();
        for (id, src) in [(1, &bad), (2, &good)] {
            assert!(worker.send(
                &ctx,
                Job::Import {
                    id,
                    src: src.clone(),
                    root: root.clone(),
                    mode: Mode::Copy,
                },
            ));
        }
        let replies = collect(&worker, 2);
        let mut imported = None;
        for reply in &replies {
            match reply {
                Reply::Read { id: 1, fields, .. } => assert!(fields.is_err()),
                Reply::Read { id: 2, fields, .. } => {
                    assert_eq!(fields.as_ref().expect("audio").title, "One");
                }
                Reply::Imported { id: 1, outcome } => assert!(outcome.is_err()),
                Reply::Imported { id: 2, outcome } => imported = Some(outcome.clone()),
                _ => {}
            }
        }
        let Some(Ok(Outcome::Imported(placed))) = imported else {
            panic!("row 2 was not imported: {imported:?}");
        };
        assert_eq!(
            placed.location.as_deref(),
            Some(
                root.join("Unknown Artist/Unknown Album/01 One.wav")
                    .as_path()
            )
        );
    }
}
