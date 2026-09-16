//! The engine thread: the only place in Phoebus that touches rodio.
//!
//! Owns one `MixerDeviceSink` and one `Player` at a time. Everything the outside world
//! can do arrives as a [`Command`] and everything it learns leaves as an [`Event`].
//!
//! The sink is bound to one OS device, never to "whatever is the default". When that
//! device disappears (earphones unplugged) the stream stops pulling samples and reports
//! `DeviceNotAvailable`; when the user picks another output the OS default moves while the
//! stream stays put. Both are handled the same way: open the current default device, build
//! a fresh player on it, and put the loaded track back where it was.

use std::fs::File;
use std::io::BufReader;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use rodio::cpal::StreamError;
use rodio::cpal::traits::{DeviceTrait, HostTrait};
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};

use crate::state::EngineState;
use crate::{Command, Event, EventKind};

/// How long the thread parks waiting for a command before looking at the player again.
const TICK: Duration = Duration::from_millis(120);
/// Minimum gap between `Progress` events while playing (~4 Hz with the tick above).
const PROGRESS_PERIOD: Duration = Duration::from_millis(250);
/// Every this many ticks the engine asks the OS which output device is the default (~1 s).
const DEVICE_POLL_TICKS: u32 = 8;
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
    let (lost_tx, lost_rx) = crossbeam_channel::unbounded();
    let output = match open_output(&lost_tx) {
        Ok(output) => output,
        Err(err) => {
            let _ = init_tx.send(Err(format!(
                "could not open the audio output device: {err}"
            )));
            return;
        }
    };
    log::info!("audio: output device {}", output.device);

    let mut engine = Engine {
        player: Player::connect_new(output.sink.mixer()),
        output,
        track: None,
        state: EngineState::new(INITIAL_UI_VOLUME),
        lost_tx,
        lost_rx,
        reopen_pending: false,
        ticks: 0,
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
    let Engine { player, output, .. } = engine;
    // The Player goes before its sink, which is the required order.
    drop(player);
    release(output.sink);
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

/// An open sink and the name of the device it is bound to.
struct Output {
    sink: MixerDeviceSink,
    device: String,
}

/// Open a sink on the OS's current default output device. Stream errors — the device
/// vanished, above all — are forwarded to `lost_tx` for the engine loop to act on.
fn open_output(lost_tx: &Sender<StreamError>) -> Result<Output, String> {
    let device = rodio::cpal::default_host()
        .default_output_device()
        .ok_or("no output device")?;
    let name = device_name(&device);
    let lost_tx = lost_tx.clone();
    let mut sink = DeviceSinkBuilder::from_device(device)
        .map_err(|err| err.to_string())?
        .with_error_callback(move |err| {
            let _ = lost_tx.send(err);
        })
        .open_sink_or_fallback()
        .map_err(|err| err.to_string())?;
    sink.log_on_drop(false);
    Ok(Output { sink, device: name })
}

/// The name of the OS's current default output device, if it has one.
fn default_output_name() -> Option<String> {
    let device = rodio::cpal::default_host().default_output_device()?;
    Some(device_name(&device))
}

fn device_name(device: &rodio::Device) -> String {
    device
        .description()
        .map(|desc| desc.name().to_string())
        .unwrap_or_else(|_| "unnamed device".to_string())
}

/// Drop a sink on a thread of its own. Disposing a CoreAudio unit whose device is gone can
/// block forever; there it stalls nothing, and the process exits without joining it.
fn release(sink: MixerDeviceSink) {
    let _ = std::thread::Builder::new()
        .name("phoebus-audio-release".to_string())
        .spawn(move || drop(sink));
}

fn decode(path: &Path) -> Result<Decoder<BufReader<File>>, String> {
    let file = File::open(path).map_err(|err| err.to_string())?;
    Decoder::try_from(file).map_err(|err| err.to_string())
}

struct Engine {
    player: Player,
    output: Output,
    /// The file behind the loaded track; only meaningful while `state.is_loaded()`.
    track: Option<PathBuf>,
    state: EngineState,
    lost_tx: Sender<StreamError>,
    lost_rx: Receiver<StreamError>,
    /// The sink must be replaced at the next device poll: its device failed, or the
    /// device the OS switched to could not be opened yet.
    reopen_pending: bool,
    ticks: u32,
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

            self.follow_output();

            // Ended detection: `empty()` alone cannot tell an end from a stop, so the
            // state machine keeps the suppression flag.
            if self.state.observe_queue(self.player.empty()) {
                self.emit(EventKind::Ended);
            }

            if self.state.is_playing() && self.last_progress.elapsed() >= PROGRESS_PERIOD {
                self.emit_progress();
            }
        }
    }

    /// Reopen the sink at once when its device failed, and once a second when the OS
    /// default output moved to another device or an earlier reopen is still owed.
    fn follow_output(&mut self) {
        let mut due = false;
        if let Some(err) = self.lost_rx.try_iter().last() {
            log::warn!("audio: output device {} failed: {err}", self.output.device);
            self.reopen_pending = true;
            due = true;
        }
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks.is_multiple_of(DEVICE_POLL_TICKS) {
            due = self.reopen_pending
                || default_output_name().is_some_and(|name| name != self.output.device);
        }
        if due {
            self.reopen_output();
        }
    }

    /// Move playback to the OS's default output device: open it, build a new player on
    /// it, and put the loaded track back at the position it had.
    fn reopen_output(&mut self) {
        let output = match open_output(&self.lost_tx) {
            Ok(output) => output,
            Err(err) => {
                log::warn!("audio: could not open the default output device: {err}");
                self.reopen_pending = true;
                return;
            }
        };
        log::info!(
            "audio: output device {} -> {}",
            self.output.device,
            output.device
        );
        let pos = self.player.get_pos();
        let old = std::mem::replace(&mut self.output, output);
        self.player = Player::connect_new(self.output.sink.mixer());
        self.player.set_volume(self.state.amplitude());
        release(old.sink);
        self.reopen_pending = false;

        let Some(path) = self.track.as_ref().filter(|_| self.state.is_loaded()) else {
            return;
        };
        let decoder = match decode(path) {
            Ok(decoder) => decoder,
            Err(err) => {
                let msg = format!("{}: {err}", path.display());
                self.state.on_stop();
                self.emit(EventKind::Error(msg));
                return;
            }
        };
        self.player.append(decoder);
        let target = self.state.seek_target(pos).unwrap_or_default();
        let pos = match self.player.try_seek(target) {
            Ok(()) => target,
            Err(err) => {
                log::warn!("audio: seek to {target:?} after reopening failed: {err}");
                Duration::ZERO
            }
        };
        if self.state.is_playing() {
            self.player.play();
        } else {
            self.player.pause();
        }
        log::info!("audio: {} resumed at {pos:?}", path.display());
        self.emit_progress_at(pos);
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Load {
                path,
                autoplay,
                generation,
            } => self.load(path, autoplay, generation),
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

    fn load(&mut self, path: PathBuf, autoplay: bool, generation: u64) {
        // Tear down first: a decode failure must leave the engine idle, not half-loaded.
        // `stop()` + `append()` is the supported way to switch tracks — recreating the
        // Player is neither needed nor wanted.
        // Adopting the generation *before* anything can fail means the `Error` of a failed
        // load is attributed to this load, not to the track it replaced.
        self.state.begin_load(generation);
        self.player.stop();

        let decoder = match decode(&path) {
            Ok(decoder) => decoder,
            Err(err) => {
                self.emit(EventKind::Error(format!("{}: {err}", path.display())));
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
        self.track = Some(path);
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

    fn emit(&self, kind: EventKind) {
        // A closed event channel just means the app is gone; the command channel
        // disconnect will stop the loop on the next tick.
        let _ = self.evt_tx.send(Event::new(self.state.generation(), kind));
    }
}
