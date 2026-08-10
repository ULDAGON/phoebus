//! Phoebus playback engine.
//!
//! One dedicated thread owns the audio device and the rodio player; everything else
//! talks to it over crossbeam channels. **No rodio type is part of this crate's public
//! API**, so the rest of Phoebus never has to know how rodio 0.22 spells things.
//!
//! Every event carries the *generation* of the `Load` it belongs to, so a caller that
//! stamps a fresh generation on every load can drop the events of a track it has already
//! replaced instead of applying them to the new one.
//!
//! ```no_run
//! use phoebus_audio::{EventKind, PlayerHandle};
//!
//! let player = PlayerHandle::spawn()?;
//! player.set_volume(0.7)?;
//! let generation = 1;
//! player.load("/music/track.m4a", true, generation)?;
//! for event in player.events().try_iter() {
//!     if event.generation != generation {
//!         continue; // belongs to a track we already replaced
//!     }
//!     match event.kind {
//!         EventKind::Loaded { duration, seekable } => println!("{duration:?} seek={seekable}"),
//!         EventKind::Progress { pos } => println!("{pos:?}"),
//!         EventKind::Ended => println!("next track please"),
//!         EventKind::SeekFailed { pos, message } => println!("still at {pos:?}: {message}"),
//!         EventKind::Error(msg) => eprintln!("{msg}"),
//!     }
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```

#![forbid(unsafe_code)]

mod engine;
mod state;

use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use crossbeam_channel::{Receiver, Sender};

/// A request to the engine thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Replace whatever is loaded with `path`, decoding it fresh.
    ///
    /// Answered by `Loaded` + a `Progress` at 0, or by `Error` if the file cannot be
    /// opened or decoded (in which case the engine falls back to idle).
    ///
    /// `generation` is chosen by the caller and echoed back on every [`Event`] the engine
    /// emits for this source, until the next `Load` replaces it. Two loads must never
    /// share a generation, or stale-event filtering stops working.
    Load {
        path: PathBuf,
        autoplay: bool,
        generation: u64,
    },
    /// Resume. No-op when nothing is loaded.
    Play,
    /// Pause, keeping the position. No-op when nothing is loaded.
    Pause,
    /// Drop the loaded track and go idle. No `Ended` is emitted.
    Stop,
    /// Seek within the current track. The target is clamped to one second before the
    /// end; a seek with nothing loaded is ignored.
    SeekTo(Duration),
    /// Set the UI volume, `0.0..=1.0`. Out-of-range values are clamped, `NaN` becomes
    /// `0.0`. A perceptual curve is applied inside the engine.
    SetVolume(f32),
}

/// Something the engine thread observed, stamped with the generation it belongs to.
///
/// The generation is the one the [`Command::Load`] that produced this source carried; it
/// is `0` until the first `Load`. Compare it against the generation of the load you are
/// waiting on and drop anything that does not match — that is the only way to tell an
/// `Ended`/`Progress` for the *outgoing* track from one for the track you just started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Generation of the `Load` this event describes.
    pub generation: u64,
    /// What actually happened.
    pub kind: EventKind,
}

impl Event {
    /// Build an event for `generation`.
    pub fn new(generation: u64, kind: EventKind) -> Event {
        Event { generation, kind }
    }
}

/// What the engine observed, without the generation stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    /// A `Load` succeeded. `duration` is the decoder's total duration, or zero for the
    /// rare file that will not report one; `seekable` is false for exactly those files —
    /// they play, but every `SeekTo` on them is answered with `SeekFailed`.
    Loaded { duration: Duration, seekable: bool },
    /// Playback position. Emitted at ~4 Hz while playing, and once immediately after
    /// every `Load`, `Play`, `Pause` and successful `SeekTo` so the UI can snap.
    Progress { pos: Duration },
    /// The track played to its end. Emitted exactly once per track, and never as a
    /// result of `Stop` or of being replaced by another `Load`.
    Ended,
    /// A `SeekTo` was refused or failed. **The track is unharmed and still loaded** —
    /// this is not a reason to skip it. `pos` is where playback actually is, so a UI that
    /// moved its readout optimistically can snap back to the truth.
    SeekFailed { pos: Duration, message: String },
    /// A load or decode failed, or the engine thread crashed. The loaded track (if any)
    /// is gone and the engine is idle; a `Load` is the only useful thing to send next.
    Error(String),
}

/// Owner of the engine thread.
///
/// Dropping the handle shuts the engine down and joins its thread.
pub struct PlayerHandle {
    /// `Option` only so that `Drop` can close the channel before joining.
    cmd_tx: Option<Sender<Command>>,
    evt_rx: Receiver<Event>,
    thread: Option<JoinHandle<()>>,
}

impl PlayerHandle {
    /// Start the engine thread.
    ///
    /// Blocks until the audio device has been opened, and returns `Err` if it cannot
    /// be: a handle that comes back `Ok` always has a live engine behind it.
    pub fn spawn() -> anyhow::Result<Self> {
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<Command>();
        let (evt_tx, evt_rx) = crossbeam_channel::unbounded::<Event>();
        let (init_tx, init_rx) = crossbeam_channel::bounded::<Result<(), String>>(1);

        let thread = std::thread::Builder::new()
            .name("phoebus-audio".to_string())
            .spawn(move || engine::run(cmd_rx, evt_tx, init_tx))
            .context("could not spawn the audio engine thread")?;

        match init_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                cmd_tx: Some(cmd_tx),
                evt_rx,
                thread: Some(thread),
            }),
            Ok(Err(msg)) => {
                let _ = thread.join();
                Err(anyhow!(msg))
            }
            Err(_) => {
                let _ = thread.join();
                Err(anyhow!("the audio engine thread died during start-up"))
            }
        }
    }

    /// Send a command. Fails only once the engine thread is gone.
    pub fn send(&self, command: Command) -> anyhow::Result<()> {
        let tx = self
            .cmd_tx
            .as_ref()
            .ok_or_else(|| anyhow!("the audio engine is shutting down"))?;
        tx.send(command)
            .map_err(|_| anyhow!("the audio engine thread is no longer running"))
    }

    /// The stream of engine events. Poll it with `try_iter()` from a UI frame, or block
    /// on `recv()`/`recv_timeout()`.
    pub fn events(&self) -> &Receiver<Event> {
        &self.evt_rx
    }

    /// Whether the engine thread is still running.
    ///
    /// Turns false once the thread has exited — because the device vanished, because
    /// rodio panicked, or because the handle is shutting down. Drain [`events`] first and
    /// check this after: events already in the channel outlive the thread that sent them.
    ///
    /// [`events`]: PlayerHandle::events
    pub fn is_alive(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// Load `path`, replacing whatever is loaded.
    ///
    /// `generation` is echoed on every event for this track; the caller owns the counter
    /// and must never reuse a value (see [`Command::Load`]).
    pub fn load(
        &self,
        path: impl Into<PathBuf>,
        autoplay: bool,
        generation: u64,
    ) -> anyhow::Result<()> {
        self.send(Command::Load {
            path: path.into(),
            autoplay,
            generation,
        })
    }

    /// Resume playback.
    pub fn play(&self) -> anyhow::Result<()> {
        self.send(Command::Play)
    }

    /// Pause, keeping the position.
    pub fn pause(&self) -> anyhow::Result<()> {
        self.send(Command::Pause)
    }

    /// Unload the current track.
    pub fn stop(&self) -> anyhow::Result<()> {
        self.send(Command::Stop)
    }

    /// Seek within the current track.
    pub fn seek_to(&self, pos: Duration) -> anyhow::Result<()> {
        self.send(Command::SeekTo(pos))
    }

    /// Set the UI volume, `0.0..=1.0`.
    pub fn set_volume(&self, ui_volume: f32) -> anyhow::Result<()> {
        self.send(Command::SetVolume(ui_volume))
    }
}

impl std::fmt::Debug for PlayerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlayerHandle")
            .field("alive", &self.is_alive())
            .field("pending_events", &self.evt_rx.len())
            .finish()
    }
}

impl Drop for PlayerHandle {
    fn drop(&mut self) {
        // Closing the command channel is the shutdown signal; the engine notices on its
        // next tick (<= ~120 ms). The event receiver stays alive until after the join,
        // so the engine can never block on a send while we wait for it.
        self.cmd_tx = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
