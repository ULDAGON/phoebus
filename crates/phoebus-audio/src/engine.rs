//! The engine thread: the only place in Phoebus that touches rodio.
//!
//! Owns one `MixerDeviceSink` and one `Player` for its whole lifetime. Everything the
//! outside world can do arrives as a [`Command`] and everything it learns leaves as an
//! [`Event`]; see docs/API-FACTS.md §1 for why each rodio call is shaped the way it is.

use std::fs::File;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use rodio::{Decoder, DeviceSinkBuilder, Player, Source};

use crate::state::EngineState;
use crate::{Command, Event, EventKind};

/// How long the thread parks waiting for a command before looking at the player again.
const TICK: Duration = Duration::from_millis(120);
/// Minimum gap between `Progress` events while playing (~4 Hz with the tick above).
const PROGRESS_PERIOD: Duration = Duration::from_millis(250);
/// UI volume the engine starts at; the app overrides it with `SetVolume` right away.
const INITIAL_UI_VOLUME: f32 = 1.0;

/// Entry point of the engine thread.
///
/// Reports the result of opening the audio device over `init_tx` before entering the
/// loop, so `PlayerHandle::spawn` can fail properly instead of handing back a dead
/// handle.
pub(crate) fn run(
    cmd_rx: Receiver<Command>,
    evt_tx: Sender<Event>,
    init_tx: Sender<Result<(), String>>,
) {
    // The MixerDeviceSink must outlive every Player: dropping it kills all audio.
    let mut device = match DeviceSinkBuilder::open_default_sink() {
        Ok(device) => device,
        Err(err) => {
            let _ = init_tx.send(Err(format!(
                "could not open the audio output device: {err}"
            )));
            return;
        }
    };
    device.log_on_drop(false);

    let mut engine = Engine {
        player: Player::connect_new(device.mixer()),
        state: EngineState::new(INITIAL_UI_VOLUME),
        evt_tx,
        last_progress: Instant::now(),
    };
    engine.player.set_volume(engine.state.amplitude());

    if init_tx.send(Ok(())).is_err() {
        return; // the spawner gave up on us
    }
    drop(init_tx);

    // The engine thread must never take the process down: turn even an unforeseen
    // panic inside rodio into an `Error` event and shut down tidily. The thread is gone
    // afterwards either way, which `PlayerHandle::is_alive` reports.
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| engine.main_loop(&cmd_rx)));
    if let Err(payload) = outcome {
        let msg = panic_message(payload.as_ref());
        log::error!("audio engine thread panicked: {msg}");
        engine.emit(EventKind::Error(format!("the audio engine crashed: {msg}")));
    }

    engine.player.stop();
    // `device` drops here, after the Player, which is the required order.
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

struct Engine {
    player: Player,
    state: EngineState,
    evt_tx: Sender<Event>,
    last_progress: Instant,
}

impl Engine {
    fn main_loop(&mut self, cmd_rx: &Receiver<Command>) {
        loop {
            match cmd_rx.recv_timeout(TICK) {
                Ok(cmd) => self.handle(cmd),
                Err(RecvTimeoutError::Timeout) => {}
                // Every `PlayerHandle` is gone: shut down.
                Err(RecvTimeoutError::Disconnected) => return,
            }

            // Ended detection: `empty()` alone cannot tell an end from a stop, so the
            // state machine keeps the suppression flag (docs/API-FACTS.md §1).
            if self.state.observe_queue(self.player.empty()) {
                self.emit(EventKind::Ended);
            }

            if self.state.wants_progress() && self.last_progress.elapsed() >= PROGRESS_PERIOD {
                self.emit_progress();
            }
        }
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Load {
                path,
                autoplay,
                generation,
            } => self.load(&path, autoplay, generation),
            Command::Play => {
                if self.state.on_play() {
                    self.player.play();
                    self.emit_progress();
                }
            }
            Command::Pause => {
                if self.state.on_pause() {
                    self.player.pause();
                    self.emit_progress();
                }
            }
            Command::Stop => {
                self.state.on_stop();
                // Leaves the player un-paused and reusable; the next `Load` sets the
                // play/pause state explicitly, so that is fine.
                self.player.stop();
            }
            Command::SeekTo(pos) => self.seek(pos),
            Command::SetVolume(ui_volume) => {
                let amplitude = self.state.set_volume(ui_volume);
                self.player.set_volume(amplitude);
            }
        }
    }

    fn load(&mut self, path: &Path, autoplay: bool, generation: u64) {
        // Tear down first: a decode failure must leave the engine idle, not half-loaded.
        // `stop()` + `append()` is the supported way to switch tracks — recreating the
        // Player is neither needed nor wanted (docs/API-FACTS.md §1).
        // Adopting the generation *before* anything can fail means the `Error` of a failed
        // load is attributed to this load, not to the track it replaced.
        self.state.begin_load(generation);
        self.player.stop();

        let file = match File::open(path) {
            Ok(file) => file,
            Err(err) => {
                self.emit_error(path, &err.to_string());
                return;
            }
        };
        let decoder = match Decoder::try_from(file) {
            Ok(decoder) => decoder,
            Err(err) => {
                self.emit_error(path, &err.to_string());
                return;
            }
        };

        let duration = decoder.total_duration();
        if duration.is_none() {
            // Verified `Some` for every seeded format; if a file ever refuses, play it
            // anyway but refuse to seek it rather than risk the past-end corruption.
            log::warn!(
                "{}: decoder reports no total duration; seeking disabled",
                path.display()
            );
        }

        self.player.append(decoder);
        // Volume lives on the Player and survives stop()+append(), but re-applying is
        // free and keeps a track switch from ever being audible at the wrong level.
        self.player.set_volume(self.state.amplitude());
        self.state.on_loaded(duration, autoplay);
        if autoplay {
            self.player.play();
        } else {
            self.player.pause();
        }

        self.emit(EventKind::Loaded {
            duration: duration.unwrap_or_default(),
            seekable: self.state.seekable(),
        });
        // NOT `get_pos()`: rodio only refreshes the shared position from the source's
        // first `periodic_access` (~5 ms of audio later), and `stop()` leaves the
        // outgoing track's position behind, so reading it here yields the *previous*
        // track's position. A freshly appended decoder is at zero by definition.
        self.emit_progress_at(Duration::ZERO);
    }

    /// Every rejected `SeekTo` answers with exactly one `SeekFailed` carrying the real
    /// position. It is deliberately **not** an `Error`: the track is still loaded and
    /// still playing, so a caller that skips tracks on `Error` must not skip on this.
    fn seek(&mut self, requested: Duration) {
        if !self.state.is_loaded() {
            self.emit_seek_failed(Duration::ZERO, "nothing is loaded".to_string());
            return;
        }
        let Some(target) = self.state.seek_target(requested) else {
            let pos = self.player.get_pos();
            self.emit_seek_failed(
                pos,
                "this track reports no duration, so seeking is disabled".to_string(),
            );
            return;
        };
        if self.player.empty() {
            // The track just finished and we have not ticked yet. `try_seek` on an
            // empty player stashes the order and applies it to the *next* source.
            let pos = self.player.get_pos();
            self.emit_seek_failed(pos, "the track already finished".to_string());
            return;
        }
        match self.player.try_seek(target) {
            // Snap the UI immediately instead of waiting for the next progress tick.
            Ok(()) => self.emit_progress(),
            Err(err) => {
                let pos = self.player.get_pos();
                self.emit_seek_failed(pos, format!("seek to {target:?} failed: {err}"));
            }
        }
    }

    fn emit_progress(&mut self) {
        let pos = self.player.get_pos();
        self.emit_progress_at(pos);
    }

    fn emit_progress_at(&mut self, pos: Duration) {
        self.last_progress = Instant::now();
        self.emit(EventKind::Progress { pos });
    }

    fn emit_seek_failed(&self, pos: Duration, message: String) {
        log::debug!("seek refused ({message}); still at {pos:?}");
        self.emit(EventKind::SeekFailed { pos, message });
    }

    fn emit_error(&self, path: &Path, msg: &str) {
        self.emit(EventKind::Error(format!("{}: {msg}", path.display())));
    }

    fn emit(&self, kind: EventKind) {
        // A closed event channel just means the app is gone; the command channel
        // disconnect will stop the loop on the next tick.
        let _ = self.evt_tx.send(Event::new(self.state.generation(), kind));
    }
}
