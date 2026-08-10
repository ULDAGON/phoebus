//! Device-level verification of the playback engine (gate G1).
//!
//! ```text
//! cargo run -p phoebus-audio --example probe -- "$HOME/.phoebus/HOME/Odyssey/01 Intro.m4a"
//! ```
//!
//! Everything runs at UI volume 0.0, so it is safe to run next to a human. Prints one
//! `PASS <check>` / `FAIL <check>` line per check and exits non-zero if any failed.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use phoebus_audio::{Command, Event, EventKind, PlayerHandle};

/// How far a reported position may drift from where we asked it to be.
const TOLERANCE: Duration = Duration::from_millis(1000);
/// Window we collect events over after issuing a seek.
const SETTLE: Duration = Duration::from_millis(350);

fn main() -> ExitCode {
    let Some(path) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: probe <path to an audio file>");
        return ExitCode::FAILURE;
    };
    println!("probe: {}", path.display());

    let player = match PlayerHandle::spawn() {
        Ok(player) => {
            println!("PASS spawn  engine thread up, audio device open");
            player
        }
        Err(err) => {
            println!("FAIL spawn  {err}");
            return ExitCode::FAILURE;
        }
    };

    let mut probe = Probe::new(player);
    probe.run(&path);
    probe.finish()
}

struct Probe {
    player: PlayerHandle,
    events: Receiver<Event>,
    failures: usize,
    errors: Vec<String>,
    /// The generation of the last `Load` the probe issued. Every event the engine emits
    /// for that track echoes it; anything else belongs to a track we already replaced.
    generation: u64,
    /// Every generation seen since the last `load`, in arrival order.
    seen_generations: Vec<u64>,
}

impl Probe {
    fn new(player: PlayerHandle) -> Self {
        let events = player.events().clone();
        Self {
            player,
            events,
            failures: 0,
            errors: Vec::new(),
            generation: 0,
            seen_generations: Vec::new(),
        }
    }

    /// Issue a `Load` under a fresh generation.
    fn load(&mut self, path: &Path, autoplay: bool) {
        self.generation += 1;
        let generation = self.generation;
        self.send(Command::Load {
            path: path.to_path_buf(),
            autoplay,
            generation,
        });
    }

    fn run(&mut self, path: &Path) {
        // Silence first. Commands are processed in order, so this lands before any
        // decoder is appended and nothing is ever audible.
        if let Err(err) = self.player.set_volume(0.0) {
            self.check("volume_zero", false, err.to_string());
            return;
        }
        self.check("volume_zero", true, "UI volume 0.0".to_string());

        // ---- load -------------------------------------------------------------------
        self.load(path, true);
        let Some((duration, seekable)) = self.await_loaded(Duration::from_secs(10)) else {
            self.check("loaded", false, "no Loaded event within 10s".to_string());
            return;
        };
        if duration < Duration::from_secs(15) {
            self.check(
                "loaded",
                false,
                format!("{} — the probe needs a track of 15s+", fmt(duration)),
            );
            return;
        }
        self.check("loaded", true, format!("duration = {}", fmt(duration)));
        // A file with a known duration is one the engine is willing to seek; the app draws
        // its scrubber live only when this is true.
        self.check(
            "loaded_reports_seekable",
            seekable,
            format!("seekable = {seekable} for a {} track", fmt(duration)),
        );
        // Every event so far belongs to the load we just issued.
        let stamped = self.only_current_generation();
        self.check(
            "generation_echoed",
            stamped,
            format!("all events stamped generation {}", self.generation),
        );

        // ---- plays for a second -----------------------------------------------------
        let played = self.collect_for(Duration::from_millis(1200));
        let ticks = played.iter().filter(|e| is_progress(e)).count();
        let pos = last_pos(&played).unwrap_or_default();
        self.check(
            "play_1s",
            (Duration::from_millis(600)..=Duration::from_millis(2200)).contains(&pos),
            format!("get_pos = {}", fmt(pos)),
        );
        self.check(
            "progress_rate",
            (3..=10).contains(&ticks),
            format!("{ticks} Progress events in 1.2s (~4 Hz expected)"),
        );

        // ---- pause freezes the position ----------------------------------------------
        self.flush();
        self.send(Command::Pause);
        let paused_at = self.await_progress(Duration::from_secs(2));
        self.collect_for(Duration::from_millis(700));
        self.flush();
        self.send(Command::Pause); // idempotent — just re-reports the position
        let still_at = self.await_progress(Duration::from_secs(2));
        match (paused_at, still_at) {
            (Some(a), Some(b)) => self.check(
                "pause_freezes",
                diff(a, b) <= Duration::from_millis(100),
                format!("{} -> {} after 700ms paused", fmt(a), fmt(b)),
            ),
            _ => self.check(
                "pause_freezes",
                false,
                "no Progress after Pause".to_string(),
            ),
        }

        // ---- resume advances again ----------------------------------------------------
        let before = still_at.unwrap_or_default();
        self.flush();
        self.send(Command::Play);
        let resumed = self.collect_for(Duration::from_millis(900));
        let after = last_pos(&resumed).unwrap_or_default();
        self.check(
            "resume_advances",
            after >= before + Duration::from_millis(400),
            format!("{} -> {} after 900ms playing", fmt(before), fmt(after)),
        );

        // ---- seek forward to 25% ------------------------------------------------------
        let quarter = duration / 4;
        let at_quarter = self.seek_and_read(quarter);
        self.check(
            "seek_forward_25pct",
            near(at_quarter, quarter),
            format!(
                "target {} -> get_pos {}",
                fmt(quarter),
                fmt(at_quarter.unwrap_or_default())
            ),
        );

        // ---- seek backward to 10% ------------------------------------------------------
        let tenth = duration / 10;
        let at_tenth = self.seek_and_read(tenth);
        let went_back = matches!((at_tenth, at_quarter), (Some(a), Some(b)) if a < b);
        self.check(
            "seek_backward_10pct",
            near(at_tenth, tenth) && went_back,
            format!(
                "target {} -> get_pos {} (backward from {})",
                fmt(tenth),
                fmt(at_tenth.unwrap_or_default()),
                fmt(at_quarter.unwrap_or_default())
            ),
        );

        // ---- run into the natural end --------------------------------------------------
        let near_end = duration.saturating_sub(Duration::from_secs(3));
        let at_end = self.seek_and_read(near_end);
        self.check(
            "seek_near_end",
            near(at_end, near_end),
            format!(
                "target {} -> get_pos {}",
                fmt(near_end),
                fmt(at_end.unwrap_or_default())
            ),
        );

        let started = Instant::now();
        let ended = self
            .await_event(Duration::from_secs(12), |e| {
                matches!(e.kind, EventKind::Ended)
            })
            .is_some();
        self.check(
            "ended_emitted",
            ended,
            format!("after {}", fmt(started.elapsed())),
        );
        let tail = self.collect_for(Duration::from_millis(2000));
        let extra = tail
            .iter()
            .filter(|e| matches!(e.kind, EventKind::Ended))
            .count();
        self.check(
            "ended_exactly_once",
            ended && extra == 0,
            format!("{extra} further Ended event(s) in the next 2s"),
        );

        // ---- a past-the-end seek is clamped, not left to corrupt get_pos ----------------
        // Loading here also proves stop()+append still works after a natural drain.
        self.load(path, true);
        if self.await_loaded(Duration::from_secs(10)).is_none() {
            self.check("reload_after_end", false, "no Loaded event".to_string());
            return;
        }
        self.check("reload_after_end", true, "loaded again".to_string());
        let absurd = duration + Duration::from_secs(60);
        let clamped = self.seek_and_read(absurd);
        let clamped_ok =
            clamped.is_some_and(|p| p < duration && p + Duration::from_secs(3) > duration);
        self.check(
            "seek_past_end_clamped",
            clamped_ok,
            format!(
                "asked {} -> get_pos {} (duration {})",
                fmt(absurd),
                fmt(clamped.unwrap_or_default()),
                fmt(duration)
            ),
        );

        // ---- Stop is not an Ended --------------------------------------------------------
        self.load(path, true);
        if self.await_loaded(Duration::from_secs(10)).is_none() {
            self.check("stop_is_not_ended", false, "no Loaded event".to_string());
            return;
        }
        self.collect_for(Duration::from_millis(400));
        self.flush();
        self.send(Command::Stop);
        let after_stop = self.collect_for(Duration::from_millis(1500));
        let spurious = after_stop
            .iter()
            .filter(|e| matches!(e.kind, EventKind::Ended))
            .count();
        self.check(
            "stop_is_not_ended",
            spurious == 0,
            format!("{spurious} Ended event(s) in the 1.5s after Stop"),
        );

        // ---- a refused seek is a SeekFailed, never an Error -------------------------------
        // Nothing is loaded after that Stop, so the engine must refuse this seek — and it
        // must say so with the non-fatal signal. An `Error` here is what used to make the
        // controller treat a bad seek as a dead file and skip the track.
        self.flush();
        self.send(Command::SeekTo(Duration::from_secs(30)));
        let refused = self.collect_for(Duration::from_millis(600));
        let seek_failed = refused
            .iter()
            .filter(|e| matches!(e.kind, EventKind::SeekFailed { .. }))
            .count();
        let seek_errors = refused
            .iter()
            .filter(|e| matches!(e.kind, EventKind::Error(_)))
            .count();
        self.errors.truncate(self.errors.len() - seek_errors); // reported by this check
        self.check(
            "refused_seek_is_not_an_error",
            seek_failed == 1 && seek_errors == 0,
            format!(
                "{seek_failed} SeekFailed, {seek_errors} Error in the 600ms after a refused seek"
            ),
        );

        // ---- load paused, then seek while paused (what `phoebus --shot` needs) -----------
        self.load(path, false);
        if self.await_loaded(Duration::from_secs(10)).is_none() {
            self.check("load_paused", false, "no Loaded event".to_string());
            return;
        }
        let idle = self.collect_for(Duration::from_millis(800));
        let idle_pos = last_pos(&idle).unwrap_or_default();
        self.check(
            "load_paused",
            idle_pos <= Duration::from_millis(100),
            format!("get_pos = {} after 800ms not playing", fmt(idle_pos)),
        );

        let thirty = Duration::from_secs(30);
        let at_thirty = self.seek_and_read(thirty);
        self.check(
            "seek_while_paused",
            near(at_thirty, thirty),
            format!(
                "target {} -> get_pos {}",
                fmt(thirty),
                fmt(at_thirty.unwrap_or_default())
            ),
        );

        let from = at_thirty.unwrap_or_default();
        self.flush();
        self.send(Command::Play);
        let rolling = self.collect_for(Duration::from_millis(800));
        let to = last_pos(&rolling).unwrap_or_default();
        self.check(
            "play_after_paused_seek",
            to >= from + Duration::from_millis(300),
            format!("{} -> {} after 800ms playing", fmt(from), fmt(to)),
        );
        self.send(Command::Stop);

        // ---- a bad path is an Error and the engine survives it ---------------------------
        self.flush();
        let expected_errors = self.errors.len();
        self.load(&PathBuf::from("/nonexistent/phoebus-probe.m4a"), true);
        let failed = self.await_event(Duration::from_secs(3), |e| {
            matches!(e.kind, EventKind::Error(_))
        });
        self.errors.truncate(expected_errors); // that one was on purpose
        self.check("bad_path_errors", failed.is_some(), String::new());
        // The failed load owns the generation it was issued under, so an app that filters
        // on generation still sees the failure of the track it just asked for.
        self.check(
            "failed_load_keeps_its_generation",
            failed
                .as_ref()
                .is_some_and(|e| e.generation == self.generation),
            format!(
                "Error stamped {:?}, current generation {}",
                failed.as_ref().map(|e| e.generation),
                self.generation
            ),
        );

        // ---- nothing from a previous generation survives a new Load ----------------------
        // Play for a while so the channel really has stale Progress in it, then replace the
        // track without draining and watch for an old stamp arriving after a new one.
        self.load(path, true);
        let alive = self.await_loaded(Duration::from_secs(10)).is_some();
        self.check(
            "engine_alive_after_error",
            alive,
            "reloaded a good file".to_string(),
        );
        self.collect_for(Duration::from_millis(900));
        let previous = self.generation;
        self.load(path, true);
        self.collect_for(Duration::from_millis(900));
        let ordered = self.generations_never_go_backwards();
        self.check(
            "no_events_from_a_previous_generation",
            ordered,
            format!(
                "generation {previous} -> {} in {:?}",
                self.generation, self.seen_generations
            ),
        );
        if let Err(err) = self.player.stop() {
            self.errors.push(err.to_string());
        }

        // The liveness predicate the app polls every frame to notice a dead engine.
        self.check(
            "handle_reports_alive",
            self.player.is_alive(),
            "PlayerHandle::is_alive() after a decode error, a drain and six loads".to_string(),
        );

        let unexpected = self.errors.len();
        self.check(
            "no_unexpected_errors",
            unexpected == 0,
            format!("{unexpected} unexpected Error event(s) {:?}", self.errors),
        );
    }

    // -- plumbing -----------------------------------------------------------------------

    fn send(&mut self, command: Command) {
        if let Err(err) = self.player.send(command) {
            self.errors.push(err.to_string());
        }
    }

    /// Issue a seek and report the position it landed on. Reads the *last* position in a
    /// short settle window so a stale periodic `Progress` cannot be mistaken for the
    /// engine's post-seek snap.
    fn seek_and_read(&mut self, target: Duration) -> Option<Duration> {
        self.flush();
        self.send(Command::SeekTo(target));
        let seen = self.collect_for(SETTLE);
        last_pos(&seen)
    }

    fn record(&mut self, event: &Event) {
        self.seen_generations.push(event.generation);
        if let EventKind::Error(msg) = &event.kind {
            self.errors.push(msg.clone());
        }
    }

    /// True when every event seen since the last `load` carries the current generation.
    fn only_current_generation(&self) -> bool {
        self.seen_generations.iter().all(|g| *g == self.generation)
    }

    /// True when the stamps arrived in non-decreasing order — i.e. once an event of the
    /// new generation showed up, nothing from the outgoing track followed it.
    fn generations_never_go_backwards(&self) -> bool {
        self.seen_generations.is_sorted()
    }

    /// Drop everything currently queued.
    fn flush(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.record(&event);
        }
    }

    /// Collect every event that arrives over `window`.
    fn collect_for(&mut self, window: Duration) -> Vec<Event> {
        let deadline = Instant::now() + window;
        let mut seen = Vec::new();
        loop {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return seen;
            };
            match self.events.recv_timeout(left) {
                Ok(event) => {
                    self.record(&event);
                    seen.push(event);
                }
                Err(_) => return seen,
            }
        }
    }

    fn await_event(
        &mut self,
        timeout: Duration,
        mut pred: impl FnMut(&Event) -> bool,
    ) -> Option<Event> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            let event = self.events.recv_timeout(left).ok()?;
            self.record(&event);
            if pred(&event) {
                return Some(event);
            }
        }
    }

    /// Wait for the `Loaded` of the current generation; yields `(duration, seekable)`.
    fn await_loaded(&mut self, timeout: Duration) -> Option<(Duration, bool)> {
        let event = self.await_event(timeout, |e| matches!(e.kind, EventKind::Loaded { .. }))?;
        match event.kind {
            EventKind::Loaded { duration, seekable } => Some((duration, seekable)),
            _ => None,
        }
    }

    fn await_progress(&mut self, timeout: Duration) -> Option<Duration> {
        match self.await_event(timeout, is_progress)?.kind {
            EventKind::Progress { pos } => Some(pos),
            _ => None,
        }
    }

    fn check(&mut self, name: &str, ok: bool, detail: String) {
        if !ok {
            self.failures += 1;
        }
        let verdict = if ok { "PASS" } else { "FAIL" };
        if detail.is_empty() {
            println!("{verdict} {name}");
        } else {
            println!("{verdict} {name}  {detail}");
        }
    }

    fn finish(self) -> ExitCode {
        if self.failures == 0 {
            println!("probe: all checks passed");
            ExitCode::SUCCESS
        } else {
            println!("probe: {} check(s) failed", self.failures);
            ExitCode::FAILURE
        }
    }
}

fn is_progress(event: &Event) -> bool {
    matches!(event.kind, EventKind::Progress { .. })
}

fn last_pos(events: &[Event]) -> Option<Duration> {
    events.iter().rev().find_map(|e| match &e.kind {
        EventKind::Progress { pos } => Some(*pos),
        _ => None,
    })
}

fn diff(a: Duration, b: Duration) -> Duration {
    a.abs_diff(b)
}

fn near(actual: Option<Duration>, target: Duration) -> bool {
    actual.is_some_and(|pos| diff(pos, target) <= TOLERANCE)
}

fn fmt(d: Duration) -> String {
    format!("{:.3}s", d.as_secs_f64())
}
