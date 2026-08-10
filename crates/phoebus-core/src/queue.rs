//! [`PlayQueue`] — Apple-Music playback semantics as pure, testable logic (no audio deps).
//!
//! The model is: a **context** (the list you double-clicked into, in its visual order), a
//! playback **order** over that context (identity, or shuffled), a **manual queue** (Play
//! Next / Play Later) that always jumps the line without moving the context position, and a
//! **history** stack so Previous works even across manual-queue jumps.
//!
//! There are three ways to start a context, and they differ only in what lands first:
//!
//! | call | first track | who calls it |
//! |------|-------------|--------------|
//! | [`PlayQueue::set_context`] | the `start` the caller named | double-clicking a row |
//! | [`PlayQueue::shuffle_play`] | uniformly random over the whole context | `SHUFFLE`, and `PLAY` while shuffle is on |
//! | [`PlayQueue::set_shuffle`]`(true)` | the track already playing | the player-bar toggle |
//!
//! Only the third pins the current track, and only because switching the toggle must not
//! cut off the song you are listening to (UI-SPEC v1.2 §Shuffle correctness).

use std::collections::VecDeque;

use rand::Rng;
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

use crate::TrackId;

/// How many played tracks are remembered for [`PlayQueue::previous`].
const HISTORY_CAP: usize = 500;

/// Repeat mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Repeat {
    /// Stop when the context runs out.
    #[default]
    Off,
    /// Wrap around at both ends of the context.
    All,
    /// A natural track end replays the same track; an explicit Next still advances.
    One,
}

impl Repeat {
    /// Cycle Off → All → One → Off (the player bar's repeat button).
    pub fn next(self) -> Repeat {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }
}

/// Why the queue is advancing — the distinction that makes [`Repeat::One`] work.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AdvanceReason {
    /// The audio engine reported the track finished on its own.
    Ended,
    /// The user pressed Next.
    UserNext,
}

/// One row of the Up Next drawer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UpNext {
    /// The track that will play.
    pub id: TrackId,
    /// True if it came from Play Next / Play Later (the UI marks these with `◆`).
    pub manual: bool,
}

/// Where an upcoming row comes from: an index into the manual queue, or into `order`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Slot {
    Manual(usize),
    Context(usize),
}

#[derive(Clone, Copy, Debug)]
struct Played {
    id: TrackId,
    was_manual: bool,
    /// The *context* row that was current at the time — an index into `context`, never into
    /// `order`.
    ///
    /// `order` is mutated underneath history (shuffle toggles permute it, `remove_upcoming`
    /// shrinks it), so an index into it goes stale the moment it is stored. `context` is only
    /// ever replaced by `set_context`, which clears history — so a context index stays
    /// meaningful for the whole life of the entry. `previous()` re-resolves it against the
    /// live `order`.
    ctx_index: Option<usize>,
}

/// The play queue. Owns no audio state: the app asks it what to play next.
#[derive(Clone, Debug, Default)]
pub struct PlayQueue {
    /// The context in its visual order.
    context: Vec<TrackId>,
    /// Playback order: indices into `context`. Shuffling permutes this; removing an
    /// upcoming context row deletes from it (so the removal survives a shuffle toggle).
    order: Vec<usize>,
    /// Index into `order` of the last context track that started playing.
    pos: Option<usize>,
    manual: VecDeque<TrackId>,
    history: VecDeque<Played>,
    current: Option<TrackId>,
    current_is_manual: bool,
    shuffle: bool,
    repeat: Repeat,
}

impl PlayQueue {
    /// An empty queue with shuffle off and repeat off.
    pub fn new() -> PlayQueue {
        PlayQueue::default()
    }

    // ---- context -------------------------------------------------------------------

    /// Make `tracks` (in their current visual order) the context and start at the track the
    /// caller **named** — double-clicking a row, or the screenshot tour asking for track 1.
    ///
    /// `start` is honoured even with shuffle on (the order becomes "`start` first, rest
    /// shuffled"): the user pointed at a song, so that song plays. When nobody pointed at a
    /// song — the `SHUFFLE` button, or `PLAY` while shuffle is on — call
    /// [`PlayQueue::shuffle_play`] instead, which draws the opener at random too.
    ///
    /// The manual queue survives (Apple Music keeps hand-queued songs when you play
    /// something else); the history does not.
    pub fn set_context(&mut self, tracks: Vec<TrackId>, start: usize) {
        if !self.adopt_context(tracks) {
            return;
        }
        let start = start.min(self.context.len() - 1);
        if self.shuffle {
            self.rebuild_shuffled(Some(start));
        } else {
            self.pos = Some(start);
        }
        self.current = Some(self.context[start]);
    }

    /// Fresh shuffle-play: make `tracks` the context, turn shuffle on, and play a
    /// **uniformly random permutation of the whole context — first track included**.
    ///
    /// This is the album/playlist `SHUFFLE` button (and `PLAY` while shuffle is already on).
    /// Nothing is pinned, so every song is equally likely to open the album, and pressing
    /// the button again re-rolls (UI-SPEC v1.2 §Shuffle correctness). Like
    /// [`PlayQueue::set_context`] it keeps the manual queue and drops the history.
    ///
    /// Returns the track to play, or `None` when `tracks` is empty.
    pub fn shuffle_play(&mut self, tracks: Vec<TrackId>) -> Option<TrackId> {
        self.shuffle_play_with(tracks, &mut rand::rng())
    }

    /// The context list, in visual order.
    pub fn context(&self) -> &[TrackId] {
        &self.context
    }

    /// The track that is (or was last) playing.
    pub fn current(&self) -> Option<TrackId> {
        self.current
    }

    /// True if the current track came from the manual queue.
    pub fn current_is_manual(&self) -> bool {
        self.current_is_manual
    }

    /// Position of the current track within the context's *visual* order, if it came from
    /// the context (used for "N of M" style displays).
    pub fn context_position(&self) -> Option<usize> {
        self.pos.and_then(|p| self.order.get(p).copied())
    }

    /// Nothing to play at all.
    pub fn is_empty(&self) -> bool {
        self.context.is_empty() && self.manual.is_empty() && self.current.is_none()
    }

    /// Forget everything: context, order, manual queue, history and current track.
    pub fn clear(&mut self) {
        let (shuffle, repeat) = (self.shuffle, self.repeat);
        *self = PlayQueue::default();
        self.shuffle = shuffle;
        self.repeat = repeat;
    }

    // ---- modes ---------------------------------------------------------------------

    /// Shuffle state.
    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    /// Turn shuffle on (current track first, everything else reshuffled) or off (linear
    /// order again, positioned at the current track so the rest of the album follows).
    ///
    /// The pin is deliberate and applies **only** to this mid-playback toggle: flipping the
    /// player-bar switch must not cut off the song that is playing. Starting a context from
    /// scratch with shuffle on is [`PlayQueue::shuffle_play`], which pins nothing.
    pub fn set_shuffle(&mut self, on: bool) {
        if on == self.shuffle {
            return;
        }
        self.shuffle = on;
        let keep = self.context_position();
        if on {
            self.rebuild_shuffled(keep);
        } else {
            self.order.sort_unstable();
            self.pos = keep.and_then(|k| self.order.iter().position(|&i| i == k));
        }
    }

    /// Toggle shuffle; returns the new state.
    pub fn toggle_shuffle(&mut self) -> bool {
        self.set_shuffle(!self.shuffle);
        self.shuffle
    }

    /// Repeat mode.
    pub fn repeat(&self) -> Repeat {
        self.repeat
    }

    /// Set the repeat mode.
    pub fn set_repeat(&mut self, repeat: Repeat) {
        self.repeat = repeat;
    }

    /// Cycle Off → All → One → Off; returns the new mode.
    pub fn cycle_repeat(&mut self) -> Repeat {
        self.repeat = self.repeat.next();
        self.repeat
    }

    // ---- manual queue --------------------------------------------------------------

    /// Queue tracks to play immediately after the current one (front of the manual queue).
    pub fn play_next(&mut self, ids: impl IntoIterator<Item = TrackId>) {
        let batch: Vec<TrackId> = ids.into_iter().collect();
        for id in batch.into_iter().rev() {
            self.manual.push_front(id);
        }
    }

    /// Queue tracks at the end of the manual queue.
    pub fn play_later(&mut self, ids: impl IntoIterator<Item = TrackId>) {
        for id in ids {
            self.manual.push_back(id);
        }
    }

    /// Number of hand-queued tracks still waiting.
    pub fn manual_len(&self) -> usize {
        self.manual.len()
    }

    /// Drop every hand-queued track (the drawer's `CLEAR` action).
    pub fn clear_manual(&mut self) {
        self.manual.clear();
    }

    // ---- transport -----------------------------------------------------------------

    /// Advance and return the track to play, or `None` if playback should stop.
    ///
    /// * [`Repeat::One`] + [`AdvanceReason::Ended`] returns the same track again (and does
    ///   not touch history, the manual queue or the context position).
    /// * The manual queue always wins over the context, and consuming a manual item leaves
    ///   the context position exactly where it was.
    /// * At the end of the context: [`Repeat::Off`] stops, [`Repeat::All`] and
    ///   [`Repeat::One`] wrap (reshuffling for the new pass when shuffle is on).
    pub fn advance(&mut self, reason: AdvanceReason) -> Option<TrackId> {
        if self.repeat == Repeat::One
            && reason == AdvanceReason::Ended
            && let Some(current) = self.current
        {
            return Some(current);
        }

        self.push_history();

        if let Some(id) = self.manual.pop_front() {
            self.current = Some(id);
            self.current_is_manual = true;
            return Some(id);
        }
        self.current_is_manual = false;

        if self.order.is_empty() {
            self.current = None;
            return None;
        }
        let next = match self.pos {
            None => Some(0),
            Some(p) if p + 1 < self.order.len() => Some(p + 1),
            Some(_) => match self.repeat {
                Repeat::Off => None,
                Repeat::All | Repeat::One => {
                    if self.shuffle {
                        let last = self.context_position();
                        self.rebuild_shuffled(None);
                        if self.order.len() > 1 && Some(self.order[0]) == last {
                            self.order.swap(0, 1);
                        }
                    }
                    Some(0)
                }
            },
        };
        match next {
            Some(p) => {
                self.pos = Some(p);
                let id = self.context[self.order[p]];
                self.current = Some(id);
                Some(id)
            }
            None => {
                self.current = None;
                None
            }
        }
    }

    /// Step back to the previously played track, or `None` if there is no history.
    ///
    /// The 3-second "restart the current track instead" rule lives in the app. If the
    /// current track came from the manual queue it is pushed back to the front of that
    /// queue, so Previous-then-Next returns exactly where you were.
    ///
    /// The remembered context row is re-resolved against the *current* `order`, so a shuffle
    /// toggle or a removed Up Next row between then and now cannot leave a dangling position
    /// behind. If that row was removed from this playthrough, the context position simply
    /// becomes unset and the next `advance` restarts the order.
    pub fn previous(&mut self) -> Option<TrackId> {
        let entry = self.history.pop_back()?;
        if let Some(c) = self.current.filter(|_| self.current_is_manual) {
            self.manual.push_front(c);
        }
        self.current = Some(entry.id);
        self.current_is_manual = entry.was_manual;
        self.pos = entry
            .ctx_index
            .and_then(|ci| self.order.iter().position(|&i| i == ci));
        Some(entry.id)
    }

    /// Whether [`PlayQueue::previous`] would return something.
    pub fn has_previous(&self) -> bool {
        !self.history.is_empty()
    }

    // ---- up next -------------------------------------------------------------------

    /// The next `n` tracks: manual queue first, then the rest of the context (wrapping
    /// once, without repeating the current track, when repeat is on).
    pub fn upcoming(&self, n: usize) -> Vec<UpNext> {
        self.upcoming_slots(n)
            .into_iter()
            .map(|slot| match slot {
                Slot::Manual(i) => UpNext {
                    id: self.manual[i],
                    manual: true,
                },
                Slot::Context(oi) => UpNext {
                    id: self.context[self.order[oi]],
                    manual: false,
                },
            })
            .collect()
    }

    /// Remove the `idx`-th row of [`PlayQueue::upcoming`]. Returns false if out of range.
    ///
    /// Removing a context row takes it out of this playthrough's order (it comes back only
    /// when a new context is set), so toggling shuffle will not resurrect it.
    pub fn remove_upcoming(&mut self, idx: usize) -> bool {
        let slots = self.upcoming_slots(idx + 1);
        let Some(&slot) = slots.get(idx) else {
            return false;
        };
        match slot {
            Slot::Manual(i) => {
                self.manual.remove(i);
            }
            Slot::Context(oi) => {
                self.order.remove(oi);
                match self.pos {
                    Some(p) if oi < p => self.pos = Some(p - 1),
                    _ => {}
                }
            }
        }
        true
    }

    /// Jump to the `idx`-th row of [`PlayQueue::upcoming`], consuming everything above it
    /// (that is what clicking a row in the Up Next drawer does). Returns the new track.
    pub fn jump_to_upcoming(&mut self, idx: usize) -> Option<TrackId> {
        let slots = self.upcoming_slots(idx + 1);
        let &slot = slots.get(idx)?;
        self.push_history();
        match slot {
            Slot::Manual(i) => {
                for _ in 0..i {
                    self.manual.pop_front();
                }
                let id = self.manual.pop_front()?;
                self.current = Some(id);
                self.current_is_manual = true;
                Some(id)
            }
            Slot::Context(oi) => {
                self.manual.clear();
                self.pos = Some(oi);
                self.current_is_manual = false;
                let id = self.context[self.order[oi]];
                self.current = Some(id);
                Some(id)
            }
        }
    }

    // ---- internals -----------------------------------------------------------------

    /// [`PlayQueue::shuffle_play`] with the randomness handed in, so the uniformity tests
    /// can seed it. Production always goes through the thread RNG.
    fn shuffle_play_with<R: Rng + ?Sized>(
        &mut self,
        tracks: Vec<TrackId>,
        rng: &mut R,
    ) -> Option<TrackId> {
        self.shuffle = true;
        if !self.adopt_context(tracks) {
            return None;
        }
        // The whole order, not "the rest": a Fisher-Yates over every index is what makes the
        // opening track uniform.
        self.order.shuffle(rng);
        self.pos = Some(0);
        self.current = Some(self.context[self.order[0]]);
        self.current
    }

    /// Adopt `tracks` as the context, resetting everything a new context invalidates and
    /// laying `order` out linearly. Returns false for an empty context, in which case there
    /// is nothing left to position on and the queue has already been emptied.
    fn adopt_context(&mut self, tracks: Vec<TrackId>) -> bool {
        self.context = tracks;
        self.history.clear();
        self.current_is_manual = false;
        if self.context.is_empty() {
            self.order.clear();
            self.pos = None;
            self.current = None;
            return false;
        }
        self.order = (0..self.context.len()).collect();
        true
    }

    fn upcoming_slots(&self, n: usize) -> Vec<Slot> {
        let mut out: Vec<Slot> = Vec::with_capacity(n.min(64));
        for i in 0..self.manual.len() {
            if out.len() >= n {
                return out;
            }
            out.push(Slot::Manual(i));
        }
        if self.order.is_empty() {
            return out;
        }
        let (linear, wrapped) = match self.pos {
            None => (0..self.order.len(), 0..0),
            Some(p) => (
                p + 1..self.order.len(),
                if self.repeat == Repeat::Off {
                    0..0
                } else {
                    0..p
                },
            ),
        };
        for oi in linear.chain(wrapped) {
            if out.len() >= n {
                break;
            }
            out.push(Slot::Context(oi));
        }
        out
    }

    fn push_history(&mut self) {
        if let Some(id) = self.current {
            self.history.push_back(Played {
                id,
                was_manual: self.current_is_manual,
                ctx_index: self.context_position(),
            });
            if self.history.len() > HISTORY_CAP {
                self.history.pop_front();
            }
        }
    }

    /// Shuffle `order` in place, optionally pinning one context index to the front.
    /// Only entries already in `order` participate, so removals are never undone.
    fn rebuild_shuffled(&mut self, keep: Option<usize>) {
        let mut rest: Vec<usize> = self
            .order
            .iter()
            .copied()
            .filter(|i| Some(*i) != keep)
            .collect();
        rest.shuffle(&mut rand::rng());
        self.order.clear();
        if let Some(k) = keep {
            self.order.push(k);
        }
        self.order.append(&mut rest);
        self.pos = keep.map(|_| 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(range: std::ops::RangeInclusive<u64>) -> Vec<TrackId> {
        range.map(TrackId).collect()
    }

    fn up(q: &PlayQueue, n: usize) -> Vec<u64> {
        q.upcoming(n).into_iter().map(|u| u.id.0).collect()
    }

    /// The whole playthrough as it will actually sound: the track that is playing, then
    /// everything still to come.
    fn play_order(q: &PlayQueue) -> Vec<u64> {
        let mut out: Vec<u64> = q.current().map(|id| id.0).into_iter().collect();
        out.extend(up(q, 1024));
        out
    }

    #[test]
    fn repeat_cycles_off_all_one() {
        let mut q = PlayQueue::new();
        assert_eq!(q.repeat(), Repeat::Off);
        assert_eq!(q.cycle_repeat(), Repeat::All);
        assert_eq!(q.cycle_repeat(), Repeat::One);
        assert_eq!(q.cycle_repeat(), Repeat::Off);
    }

    #[test]
    fn repeat_one_replays_on_ended_but_advances_on_user_next() {
        let mut q = PlayQueue::new();
        q.set_context(ids(1..=3), 0);
        q.set_repeat(Repeat::One);
        assert_eq!(q.current(), Some(TrackId(1)));
        assert_eq!(q.advance(AdvanceReason::Ended), Some(TrackId(1)));
        assert_eq!(q.advance(AdvanceReason::Ended), Some(TrackId(1)));
        // Replays must not pile up in history.
        assert!(!q.has_previous());
        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(2)));
        assert_eq!(q.advance(AdvanceReason::Ended), Some(TrackId(2)));
        assert_eq!(q.previous(), Some(TrackId(1)));
    }

    #[test]
    fn repeat_one_replays_a_manual_track_too() {
        let mut q = PlayQueue::new();
        q.set_context(ids(1..=3), 0);
        q.play_next([TrackId(9)]);
        q.set_repeat(Repeat::One);
        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(9)));
        assert_eq!(q.advance(AdvanceReason::Ended), Some(TrackId(9)));
        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(2)));
    }

    #[test]
    fn repeat_off_stops_at_the_end_of_the_context() {
        let mut q = PlayQueue::new();
        q.set_context(ids(1..=2), 1);
        assert_eq!(q.current(), Some(TrackId(2)));
        assert_eq!(q.advance(AdvanceReason::Ended), None);
        assert_eq!(q.current(), None);
        assert_eq!(q.advance(AdvanceReason::UserNext), None);
        // …but the stopped track is still in history.
        assert_eq!(q.previous(), Some(TrackId(2)));
        assert!(up(&q, 5).is_empty());
    }

    #[test]
    fn repeat_all_wraps_at_both_ends() {
        let mut q = PlayQueue::new();
        q.set_context(ids(1..=3), 2);
        q.set_repeat(Repeat::All);
        assert_eq!(q.advance(AdvanceReason::Ended), Some(TrackId(1)));
        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(2)));
        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(3)));
        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(1)));
        // Up Next wraps once and never lists the current track.
        assert_eq!(up(&q, 10), vec![2, 3]);
    }

    #[test]
    fn shuffle_on_keeps_the_current_track_and_every_other_track() {
        let mut q = PlayQueue::new();
        q.set_context(ids(1..=20), 5);
        assert_eq!(q.current(), Some(TrackId(6)));
        q.set_shuffle(true);
        assert!(q.shuffle());
        assert_eq!(q.current(), Some(TrackId(6)), "current must not change");
        assert_eq!(q.context_position(), Some(5), "still the same context row");

        let mut rest = up(&q, 100);
        assert_eq!(rest.len(), 19);
        assert!(!rest.contains(&6));
        rest.sort_unstable();
        let expect: Vec<u64> = (1..=20).filter(|n| *n != 6).collect();
        assert_eq!(rest, expect, "shuffle must not lose or duplicate tracks");
    }

    #[test]
    fn shuffle_off_restores_linear_order_at_the_current_track() {
        let mut q = PlayQueue::new();
        q.set_context(ids(1..=10), 0);
        q.set_shuffle(true);
        for _ in 0..3 {
            q.advance(AdvanceReason::UserNext);
        }
        let current = q.current().expect("playing");
        q.set_shuffle(false);
        assert_eq!(q.current(), Some(current), "current survives un-shuffle");
        let n = current.0;
        let expect: Vec<u64> = (n + 1..=10).collect();
        assert_eq!(
            up(&q, 100),
            expect,
            "linear remainder follows the current track"
        );
    }

    #[test]
    fn shuffling_an_empty_or_unstarted_queue_is_safe() {
        let mut q = PlayQueue::new();
        q.set_shuffle(true);
        assert!(q.upcoming(5).is_empty());
        q.set_context(ids(1..=4), 2);
        assert_eq!(q.current(), Some(TrackId(3)));
        assert_eq!(q.upcoming(10).len(), 3);
        q.set_shuffle(false);
        assert_eq!(up(&q, 10), vec![4]);
    }

    #[test]
    fn manual_queue_takes_priority_and_leaves_the_context_position_alone() {
        let mut q = PlayQueue::new();
        q.set_context(ids(1..=4), 0);
        q.play_next([TrackId(9)]);
        q.play_later([TrackId(10), TrackId(11)]);
        assert_eq!(up(&q, 10), vec![9, 10, 11, 2, 3, 4]);
        assert_eq!(q.manual_len(), 3);

        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(9)));
        assert!(q.current_is_manual());
        assert_eq!(q.context_position(), Some(0), "context position frozen");
        assert_eq!(q.advance(AdvanceReason::Ended), Some(TrackId(10)));
        assert_eq!(q.context_position(), Some(0));
        assert_eq!(q.advance(AdvanceReason::Ended), Some(TrackId(11)));
        assert_eq!(q.advance(AdvanceReason::Ended), Some(TrackId(2)));
        assert!(!q.current_is_manual());
        assert_eq!(q.context_position(), Some(1));
        assert_eq!(q.manual_len(), 0);
    }

    #[test]
    fn play_next_keeps_batch_order_and_jumps_the_line() {
        let mut q = PlayQueue::new();
        q.set_context(ids(1..=3), 0);
        q.play_later([TrackId(20)]);
        q.play_next([TrackId(30), TrackId(31)]);
        assert_eq!(up(&q, 10), vec![30, 31, 20, 2, 3]);
    }

    #[test]
    fn previous_walks_back_across_a_manual_jump_and_forward_again() {
        let mut q = PlayQueue::new();
        q.set_context(ids(1..=5), 0);
        q.play_next([TrackId(9)]);
        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(9)));
        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(2)));

        assert_eq!(q.previous(), Some(TrackId(9)), "back to the manual track");
        assert!(q.current_is_manual());
        assert_eq!(q.context_position(), Some(0), "context position restored");

        assert_eq!(q.previous(), Some(TrackId(1)), "back to the context track");
        assert!(!q.current_is_manual());
        assert_eq!(q.manual_len(), 1, "the manual track is re-queued");

        // Forward again reproduces the same sequence.
        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(9)));
        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(2)));
        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(3)));
    }

    #[test]
    fn previous_without_history_is_none() {
        let mut q = PlayQueue::new();
        assert_eq!(q.previous(), None);
        q.set_context(ids(1..=3), 1);
        assert!(!q.has_previous());
        assert_eq!(q.previous(), None, "app handles restart-at-zero itself");
        assert_eq!(q.current(), Some(TrackId(2)));
    }

    #[test]
    fn manual_items_play_even_without_a_context() {
        let mut q = PlayQueue::new();
        assert!(q.is_empty());
        q.play_next([TrackId(7), TrackId(8)]);
        assert!(!q.is_empty());
        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(7)));
        assert_eq!(q.advance(AdvanceReason::Ended), Some(TrackId(8)));
        assert_eq!(q.advance(AdvanceReason::Ended), None);
    }

    #[test]
    fn clear_manual_and_clear() {
        let mut q = PlayQueue::new();
        q.set_context(ids(1..=3), 0);
        q.play_later([TrackId(9)]);
        q.clear_manual();
        assert_eq!(q.manual_len(), 0);
        assert_eq!(up(&q, 10), vec![2, 3]);
        q.set_repeat(Repeat::All);
        q.clear();
        assert!(q.is_empty());
        assert_eq!(q.current(), None);
        assert_eq!(q.repeat(), Repeat::All, "modes survive a clear");
    }

    #[test]
    fn remove_upcoming_handles_manual_and_context_rows() {
        let mut q = PlayQueue::new();
        q.set_context(ids(1..=4), 0);
        q.play_later([TrackId(9), TrackId(10)]);
        assert_eq!(up(&q, 10), vec![9, 10, 2, 3, 4]);

        assert!(q.remove_upcoming(1)); // manual 10
        assert_eq!(up(&q, 10), vec![9, 2, 3, 4]);
        assert!(q.remove_upcoming(2)); // context 3
        assert_eq!(up(&q, 10), vec![9, 2, 4]);
        assert!(!q.remove_upcoming(9));

        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(9)));
        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(2)));
        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(4)));
    }

    #[test]
    fn removed_context_rows_stay_removed_across_a_shuffle_toggle() {
        let mut q = PlayQueue::new();
        q.set_context(ids(1..=5), 0);
        assert!(q.remove_upcoming(1)); // drop track 3
        q.set_shuffle(true);
        q.set_shuffle(false);
        assert_eq!(up(&q, 10), vec![2, 4, 5]);
    }

    #[test]
    fn jump_to_a_context_row_consumes_the_manual_queue() {
        let mut q = PlayQueue::new();
        q.set_context(ids(1..=4), 0);
        q.play_next([TrackId(9)]);
        assert_eq!(q.jump_to_upcoming(2), Some(TrackId(3)));
        assert_eq!(q.manual_len(), 0);
        assert_eq!(q.context_position(), Some(2));
        assert_eq!(q.previous(), Some(TrackId(1)));
    }

    #[test]
    fn jump_to_a_manual_row_drops_the_ones_above_it() {
        let mut q = PlayQueue::new();
        q.set_context(ids(1..=4), 0);
        q.play_later([TrackId(9), TrackId(10)]);
        assert_eq!(q.jump_to_upcoming(1), Some(TrackId(10)));
        assert!(q.current_is_manual());
        assert_eq!(q.manual_len(), 0);
        assert_eq!(q.context_position(), Some(0), "context untouched");
        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(2)));
        assert_eq!(q.jump_to_upcoming(9), None);
    }

    #[test]
    fn set_context_clamps_start_and_keeps_hand_queued_tracks() {
        let mut q = PlayQueue::new();
        q.play_later([TrackId(99)]);
        q.set_context(ids(1..=3), 99);
        assert_eq!(q.current(), Some(TrackId(3)));
        assert_eq!(q.manual_len(), 1, "manual queue survives a context change");
        assert!(!q.has_previous(), "history is reset by a new context");
        q.set_context(Vec::new(), 0);
        assert_eq!(q.current(), None);
        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(99)));
    }

    #[test]
    fn shuffle_before_playing_still_starts_on_the_chosen_track() {
        let mut q = PlayQueue::new();
        q.set_shuffle(true);
        q.set_context(ids(1..=10), 3);
        assert_eq!(q.current(), Some(TrackId(4)));
        assert_eq!(q.context_position(), Some(3));
        assert_eq!(q.upcoming(100).len(), 9);
    }

    /// Everything that must hold after *any* sequence of public calls.
    fn check_invariants(q: &PlayQueue) {
        let mut seen = vec![false; q.context.len()];
        for &i in &q.order {
            assert!(
                i < q.context.len(),
                "order entry {i} is not a context index (context len {})",
                q.context.len()
            );
            assert!(!seen[i], "order lists context index {i} twice");
            seen[i] = true;
        }
        if let Some(p) = q.pos {
            assert!(
                p < q.order.len(),
                "pos {p} is out of range for order len {}",
                q.order.len()
            );
        }
        // Never panics, and agrees with `pos`.
        assert_eq!(q.context_position(), q.pos.map(|p| q.order[p]));
        if !q.current_is_manual()
            && let (Some(current), Some(p)) = (q.current(), q.pos)
        {
            assert_eq!(
                current, q.context[q.order[p]],
                "current track disagrees with the context position"
            );
        }
        // Reading the drawer must never panic either.
        let _ = q.upcoming(8);
    }

    /// The reviewer's seven-click reproducer, every step of which is a real UI action:
    /// Play Later, play the last track of a 5-track album, Next, shuffle ON, Play Next,
    /// delete the third Up Next row, Previous — then press the shuffle button again.
    #[test]
    fn previous_after_removing_an_upcoming_row_survives_a_shuffle_toggle() {
        let mut q = PlayQueue::new();
        q.play_later([TrackId(9)]); // Play Later
        q.set_context(ids(1..=5), 4); // double-click the last track of the album
        assert_eq!(q.advance(AdvanceReason::UserNext), Some(TrackId(9))); // Next
        q.set_shuffle(true); // shuffle ON
        q.play_next([TrackId(8)]); // Play Next
        assert!(q.remove_upcoming(2)); // delete the 3rd Up Next row
        assert_eq!(q.previous(), Some(TrackId(5))); // Previous

        // The queue must be coherent, not merely un-panicked.
        check_invariants(&q);
        assert_eq!(q.current(), Some(TrackId(5)));
        assert_eq!(q.context_position(), Some(4), "back on the 5th context row");

        // …and the shuffle button must not blow up.
        assert!(!q.toggle_shuffle());
        check_invariants(&q);
        assert!(q.toggle_shuffle());
        check_invariants(&q);
        assert_eq!(
            q.current(),
            Some(TrackId(5)),
            "current survives both toggles"
        );
    }

    /// The same defect with shuffle never enabled: wrap under Repeat::All, delete a row,
    /// press Previous. Playback must continue, and the first shuffle press must not panic.
    #[test]
    fn previous_after_a_wrapped_removal_keeps_playing_and_shuffles_safely() {
        let mut q = PlayQueue::new();
        q.set_context(ids(1..=5), 4);
        q.set_repeat(Repeat::All);
        assert_eq!(q.advance(AdvanceReason::Ended), Some(TrackId(1)), "wrapped");
        assert!(q.remove_upcoming(0)); // drop track 2
        assert_eq!(q.previous(), Some(TrackId(5)));
        check_invariants(&q);
        assert_eq!(q.context_position(), Some(4));

        // Playback continues from where Previous put us instead of stopping dead.
        assert_eq!(up(&q, 10), vec![1, 3, 4], "removal survived, order intact");
        assert_eq!(
            q.advance(AdvanceReason::Ended),
            Some(TrackId(1)),
            "wraps on"
        );
        check_invariants(&q);

        q.set_shuffle(true);
        check_invariants(&q);
        assert_eq!(q.current(), Some(TrackId(1)));
    }

    /// The non-crashing half of the same defect: a stale position that stays *in* range
    /// still points at the wrong row, and `advance` then stops playback while a context row
    /// is queued.
    #[test]
    fn previous_after_a_removal_keeps_the_right_row_and_does_not_stop_early() {
        let mut q = PlayQueue::new();
        q.set_context(ids(1..=6), 4);
        q.set_repeat(Repeat::All);
        assert_eq!(q.advance(AdvanceReason::Ended), Some(TrackId(6)));
        assert!(q.remove_upcoming(0)); // drop the wrapped row for track 1
        assert_eq!(q.previous(), Some(TrackId(5)));
        check_invariants(&q);
        assert_eq!(
            q.context_position(),
            Some(4),
            "the row of track 5, not the row the index happened to land on"
        );
        q.set_repeat(Repeat::Off);
        assert_eq!(
            q.advance(AdvanceReason::UserNext),
            Some(TrackId(6)),
            "a context row was still queued; playback must not stop"
        );
    }

    /// 2000 seeded sessions of 40 random public-API calls each: no panic, and every
    /// invariant holds after every single call.
    #[test]
    fn a_seeded_fuzz_of_the_public_api_never_panics_and_keeps_the_invariants() {
        use rand::rngs::StdRng;
        use rand::{Rng, SeedableRng};

        let mut rng = StdRng::seed_from_u64(0x50ee_b005_1234_5678);
        for _run in 0..2000 {
            let mut q = PlayQueue::new();
            for _op in 0..40 {
                match rng.random_range(0..14u32) {
                    0 => {
                        let n = rng.random_range(0..7u64);
                        let start = rng.random_range(0..8usize);
                        q.set_context(ids(1..=n), start);
                    }
                    13 => {
                        // The SHUFFLE button. Seeded through the same rng so a failing run
                        // is reproducible from its seed alone.
                        let n = rng.random_range(0..7u64);
                        q.shuffle_play_with(ids(1..=n), &mut rng);
                    }
                    1 => q.set_shuffle(rng.random_bool(0.5)),
                    2 => {
                        q.toggle_shuffle();
                    }
                    3 => {
                        q.set_repeat(match rng.random_range(0..3u32) {
                            0 => Repeat::Off,
                            1 => Repeat::All,
                            _ => Repeat::One,
                        });
                    }
                    4 => {
                        q.advance(AdvanceReason::Ended);
                    }
                    5 => {
                        q.advance(AdvanceReason::UserNext);
                    }
                    6 => {
                        q.previous();
                    }
                    7 => {
                        q.remove_upcoming(rng.random_range(0..8usize));
                    }
                    8 => {
                        q.jump_to_upcoming(rng.random_range(0..8usize));
                    }
                    9 => q.play_next([TrackId(rng.random_range(100..110u64))]),
                    10 => q.play_later([TrackId(rng.random_range(100..110u64))]),
                    11 => q.clear_manual(),
                    _ => q.clear(),
                }
                check_invariants(&q);
            }
        }
    }

    // ---- UI-SPEC v1.2 §Shuffle correctness -----------------------------------------

    /// How many tracks the shuffle statistics run over, and how many presses of `SHUFFLE`.
    const STAT_TRACKS: u64 = 12;
    const STAT_RUNS: usize = 2000;

    fn stat_rng() -> rand::rngs::StdRng {
        use rand::SeedableRng;
        rand::rngs::StdRng::seed_from_u64(0x50ee_b005_5ee4_11e5)
    }

    /// `SHUFFLE` pressed 2000 times on the same 12-track album, exactly as a user would:
    /// every track must open the album sometimes, and no track may hog the slot.
    ///
    /// This is the regression test for the shipped bug, where the opener came from
    /// `SystemTime::now().subsec_nanos() % len`. macOS's realtime clock ticks in whole
    /// microseconds, so `subsec_nanos()` is always a multiple of 1000 and the expression can
    /// only ever produce multiples of `gcd(1000, len)`: three openers for a 12-track album,
    /// two for a 16-track one ("always track 1 or 9"), and exactly one for 4, 8, 10 or 20
    /// tracks. Uniformity here is the whole point, so it is asserted numerically rather than
    /// by eyeballing the shape.
    #[test]
    fn fresh_shuffle_play_opens_on_every_track_about_equally_often() {
        let tracks = ids(1..=STAT_TRACKS);
        let mut rng = stat_rng();
        let mut q = PlayQueue::new();
        let mut first = vec![0usize; STAT_TRACKS as usize];

        for _ in 0..STAT_RUNS {
            // Same queue every time: each iteration is "press SHUFFLE again while something
            // from this album is already playing", which is precisely where the old code
            // pinned the start track.
            let id = q
                .shuffle_play_with(tracks.clone(), &mut rng)
                .expect("a non-empty context always yields a track");
            first[(id.0 - 1) as usize] += 1;
        }

        let (min, max) = (
            *first.iter().min().expect("12 buckets"),
            *first.iter().max().expect("12 buckets"),
        );
        assert!(
            first.iter().all(|&c| c > 0),
            "every track must be able to come first, got {first:?}"
        );
        assert!(
            min > 100,
            "no track may be starved (expected ~{}), got {first:?}",
            STAT_RUNS / STAT_TRACKS as usize
        );
        assert!(
            max < 250,
            "no track may hog the opening slot (expected ~{}), got {first:?}",
            STAT_RUNS / STAT_TRACKS as usize
        );
    }

    /// Not just the first slot: every track's *mean position* over 2000 shuffles must sit
    /// within 2σ of the middle of the album. A permutation that only randomised the tail
    /// (or one that pinned a track anywhere) shifts a mean well outside that band.
    #[test]
    fn fresh_shuffle_play_spreads_every_track_over_every_position() {
        let n = STAT_TRACKS as usize;
        let tracks = ids(1..=STAT_TRACKS);
        let mut rng = stat_rng();
        let mut q = PlayQueue::new();
        let mut totals = vec![0usize; n];

        for _ in 0..STAT_RUNS {
            q.shuffle_play_with(tracks.clone(), &mut rng);
            for (position, id) in play_order(&q).into_iter().enumerate() {
                totals[(id - 1) as usize] += position;
            }
        }

        // Position of one track in one shuffle is uniform on 0..n, so its variance is
        // (n²−1)/12; the mean of `STAT_RUNS` draws has that variance divided by the count.
        let len = n as f64;
        let centre = (len - 1.0) / 2.0;
        let sigma = ((len * len - 1.0) / 12.0 / STAT_RUNS as f64).sqrt();
        for (i, &total) in totals.iter().enumerate() {
            let mean = total as f64 / STAT_RUNS as f64;
            assert!(
                (mean - centre).abs() <= 2.0 * sigma,
                "track {} averages position {mean:.3}, more than 2σ ({:.3}) from {centre:.1}",
                i + 1,
                2.0 * sigma,
            );
        }
    }

    /// "Pressing SHUFFLE again re-rolls": two presses in a row must not hand back the same
    /// running order. For 12 tracks a collision has probability 1/12! ≈ 2·10⁻⁹ per pair.
    #[test]
    fn pressing_shuffle_again_re_rolls_the_order() {
        let tracks = ids(1..=STAT_TRACKS);
        let mut rng = stat_rng();
        let mut q = PlayQueue::new();

        for press in 0..200 {
            q.shuffle_play_with(tracks.clone(), &mut rng);
            let before = play_order(&q);
            q.shuffle_play_with(tracks.clone(), &mut rng);
            let after = play_order(&q);
            assert_eq!(
                before.len(),
                STAT_TRACKS as usize,
                "the whole album is queued"
            );
            assert_ne!(after, before, "press {press} did not re-roll the order");
        }
    }

    /// The bookkeeping around the permutation: shuffle turns itself on, the album is played
    /// exactly once through, hand-queued songs survive and the old history does not.
    #[test]
    fn shuffle_play_turns_shuffle_on_and_keeps_the_manual_queue() {
        let mut q = PlayQueue::new();
        q.set_context(ids(1..=5), 0);
        q.advance(AdvanceReason::UserNext);
        q.play_later([TrackId(99)]);
        assert!(q.has_previous());

        q.shuffle_play(ids(1..=5));
        assert!(q.shuffle(), "the SHUFFLE button turns shuffle on");
        assert!(!q.has_previous(), "a new context resets history");
        assert_eq!(q.manual_len(), 1, "hand-queued songs survive");
        assert!(!q.current_is_manual());

        q.clear_manual(); // so `play_order` shows the album alone
        let mut order = play_order(&q);
        assert_eq!(order.len(), 5, "every track exactly once");
        order.sort_unstable();
        assert_eq!(order, vec![1, 2, 3, 4, 5]);

        // Whatever the permutation opened with, the queue agrees about which visual row it is.
        let row = q
            .context()
            .iter()
            .position(|id| Some(*id) == q.current())
            .expect("current came from the context");
        assert_eq!(q.context_position(), Some(row));
        check_invariants(&q);
    }

    /// An empty SHUFFLE is a no-op that leaves nothing dangling.
    #[test]
    fn shuffle_play_on_an_empty_context_is_safe() {
        let mut q = PlayQueue::new();
        assert_eq!(q.shuffle_play(Vec::new()), None);
        assert!(q.is_empty());
        check_invariants(&q);
    }

    #[test]
    fn shuffled_repeat_all_wrap_replays_every_track() {
        let mut q = PlayQueue::new();
        q.set_context(ids(1..=6), 0);
        q.set_shuffle(true);
        q.set_repeat(Repeat::All);
        let mut seen = vec![];
        for _ in 0..6 {
            seen.push(q.advance(AdvanceReason::Ended).expect("wraps forever").0);
        }
        assert_eq!(seen.len(), 6);
        // The 6th advance wrapped into a fresh pass; it must not repeat the 5th track.
        assert_ne!(seen[4], seen[5]);
    }
}
