//! The transport: which timeline sample the output plays, and its play and stop commands
//! ([ADR-0025](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0025-delay-compensation-by-reading-ahead.md)).
//!
//! The processor keeps the engine's clock, which counts every period, and beside it the
//! transport's: the timeline sample heard at the output. Each node sees the transport's clock
//! plus how far ahead of the output its input runs ([`Transport`]).
//!
//! - **Play** from `from`, stopped: the clock starts at `from - P`, `P` the compiled graph's largest
//!   compensation, so that the most latent path reads `from` at once and every path reaches the
//!   output on `from` together, `P` samples after the command's period starts. That pre-roll is
//!   published in [`EngineStats`]. The audible region is `[from, stop)`, its stop unknown.
//! - **Stop** at clock `c`: if `c + P <= from`, no node has read anything audible, so the region
//!   is empty and the transport stops at once. Otherwise every path ends on the same timeline
//!   sample, `stop = c + P + F`, `F` the 5 ms of a fade, where each node's sources end its notes
//!   as its position reaches it, or passes it in a jump (`sources.rs`). A stop in a later period of
//!   the pre-roll so plays what was read, a period at least, and the fade. The transport rolls on
//!   until its clock has played `stop` itself, so that the nodes nearest the output, which reach
//!   `stop` last, with the clock, end theirs too; its clock then rests on `stop + 1`, the next
//!   sample it would play.
//! - **Play while rolling**: a stop as above, then the new play at the first period after it ends;
//!   at once if the stop is.
//!
//! Commands come through a queue of their own and apply at a period's start, after any swap, so a
//! play pre-rolls by the compensation of the graph about to play. A swap that changes the
//! compensation while the transport rolls keeps the running clock.

// The processor runs this module's code in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use rtrb::Consumer;

use crate::node::{AudibleRegion, Transport};
use crate::stats::EngineStats;
use crate::time::SamplePosition;

/// Capacity of the queue of transport commands: a few between two periods, which pop them all.
pub(crate) const COMMAND_CAPACITY: usize = 4;

/// What the controller asks of the transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransportCommand {
    Play(Play),
    Stop,
}

/// A play from a timeline sample, numbered by the controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Play {
    pub(crate) from: SamplePosition,
    pub(crate) sequence: u64,
}

/// The processor's transport: its clock, the audible region, and a play that waits for a stop to
/// end.
pub(crate) struct TransportClock {
    commands: Consumer<TransportCommand>,
    /// The 5 ms by which a stop lets every path reach the same sample.
    fade_frames: i64,
    rolling: bool,
    /// The timeline sample the output plays at the next sub-block's first frame; stopped, the next
    /// it would play.
    clock: i64,
    /// The audible region's start.
    from: i64,
    /// The audible region's stop, once asked for.
    stop: Option<i64>,
    /// A play asked while rolling, which starts at the first period after the stop ends.
    waiting: Option<Play>,
    /// The number of the last play started; 0 before any.
    play: u64,
}

impl TransportClock {
    /// A transport stopped at sample 0, reading `commands`. Built off the real-time thread.
    pub(crate) fn new(commands: Consumer<TransportCommand>, fade_frames: usize) -> Self {
        Self {
            commands,
            fade_frames: i64::try_from(fade_frames).unwrap_or(i64::MAX),
            rolling: false,
            clock: 0,
            from: 0,
            stop: Some(0),
            waiting: None,
            play: 0,
        }
    }

    /// At a period's start, `period_start` on the engine's clock: starts a play that waited for its
    /// stop, then applies every queued command in order, for a graph whose largest compensation is
    /// `compensation`.
    pub(crate) fn apply_commands(
        &mut self,
        period_start: i64,
        compensation: u64,
        stats: &EngineStats,
    ) {
        let compensation = i64::try_from(compensation).unwrap_or(i64::MAX);
        if !self.rolling
            && let Some(play) = self.waiting.take()
        {
            self.start(play, period_start, compensation, stats);
        }
        while let Ok(command) = self.commands.pop() {
            match command {
                TransportCommand::Play(play) => {
                    if self.rolling {
                        self.ask_to_stop(compensation);
                    }
                    if self.rolling {
                        self.waiting = Some(play);
                    } else {
                        self.start(play, period_start, compensation, stats);
                    }
                }
                TransportCommand::Stop => {
                    self.waiting = None;
                    if self.rolling {
                        self.ask_to_stop(compensation);
                    }
                }
            }
        }
    }

    /// Starts a play: the clock pre-rolls by the compensation, and the engine sample at which the
    /// output reaches `from` is published.
    fn start(&mut self, play: Play, period_start: i64, compensation: i64, stats: &EngineStats) {
        self.rolling = true;
        self.play = play.sequence;
        self.from = play.from.0;
        self.stop = None;
        self.clock = play.from.0.saturating_sub(compensation);
        let audible_at = SamplePosition(period_start.saturating_add(compensation));
        stats.publish_play_start(play.sequence, audible_at);
    }

    /// Sets where a rolling transport stops, unless it is set already; stops at once if nothing
    /// audible has been read.
    fn ask_to_stop(&mut self, compensation: i64) {
        if self.stop.is_some() {
            return;
        }
        // The most latent node has read the timeline up to here, excluded.
        let read_up_to = self.clock.saturating_add(compensation);
        if read_up_to <= self.from {
            self.stop = Some(self.from);
            self.rolling = false;
        } else {
            self.stop = Some(read_up_to.saturating_add(self.fade_frames));
        }
    }

    /// The number of the last play started, by which a node's sources stop once per play; 0 before
    /// any.
    pub(crate) fn play(&self) -> u64 {
        self.play
    }

    /// How many frames the transport rolls before it stops, when it rolls towards a known stop: up
    /// to the stop's own sample, included. The processor cuts its sub-block there, so the transport
    /// never stops inside one.
    pub(crate) fn frames_until_stop(&self) -> Option<usize> {
        let stop = self.stop.filter(|_| self.rolling)?;
        usize::try_from(stop.saturating_add(1).saturating_sub(self.clock)).ok()
    }

    /// The transport as a node whose input runs `ahead_in` samples ahead of the output sees it, at
    /// the next sub-block's first frame.
    pub(crate) fn at(&self, ahead_in: u64) -> Transport {
        let ahead_in = i64::try_from(ahead_in).unwrap_or(i64::MAX);
        Transport {
            rolling: self.rolling,
            position: SamplePosition(self.clock.saturating_add(ahead_in)),
            audible: AudibleRegion {
                start: SamplePosition(self.from),
                stop: self.stop.map(SamplePosition),
            },
        }
    }

    /// Moves the clock on by a sub-block of `frames` frames, and stops once it has played the stop.
    pub(crate) fn advance(&mut self, frames: usize) {
        if !self.rolling {
            return;
        }
        let frames = i64::try_from(frames).unwrap_or(i64::MAX);
        self.clock = self.clock.saturating_add(frames);
        if self.stop.is_some_and(|stop| self.clock > stop) {
            self.rolling = false;
        }
    }
}
