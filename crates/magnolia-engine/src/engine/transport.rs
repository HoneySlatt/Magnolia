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
//! A play starts from a [`Cue`]: its timeline sample, and the generation of the disk streams it
//! reads, which the disk reader's `locate` gave, or 0 for none. Each node sees the generation of
//! the play under way in its [`Transport`], so that an audio player reads only what its stream
//! holds for that play
//! ([ADR-0026](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0026-one-stream-per-audio-track.md)).
//! A play of a generation above 0 starts only once a disk reader has published it primed
//! ([`EngineStats::primed_generation`]): it waits, as a play while rolling waits for its stop, and
//! the transport looks again at each period's start, never waiting itself.
//!
//! Commands come through a queue of their own and apply at a period's start, after any swap, so a
//! play pre-rolls by the compensation of the graph about to play. A swap that changes the
//! compensation while the transport rolls keeps the running clock.
//!
//! The capture tap places each take by the last play ([`PlayInfo`]): the engine sample at which the
//! output plays its first timeline sample, and its stop once asked
//! ([ADR-0032](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0032-recording-by-a-capture-tap-of-the-processor.md)).
//! Within a play the clock moves with the engine's, sample for sample, so the output plays timeline
//! sample `t` at engine sample `audible_at + (t - from)`.

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
    /// The generation of the disk streams it reads; 0 for none.
    pub(crate) generation: u64,
    pub(crate) sequence: u64,
}

/// Where a play starts: a timeline sample, and the generation of the disk streams it reads
/// ([ADR-0026](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0026-one-stream-per-audio-track.md)).
///
/// [`DiskReader::locate`](crate::disk::DiskReader::locate) gives a cue whose streams it fills from
/// that sample, of a generation the engine numbers; [`Controller::play`](super::Controller::play)
/// plays it, once the reader has primed it. A sample alone is a cue of generation 0, which no
/// stream ever holds: enough for MIDI, silent for audio players, which count what they lack as
/// underruns. Only the cue of the reader's last locate plays its streams, and once: the controller
/// refuses a cue of a generation it has sent already, or older.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cue {
    start: SamplePosition,
    generation: u64,
}

impl Cue {
    /// A cue of a generation the disk reader has located.
    pub(crate) fn new(start: SamplePosition, generation: u64) -> Self {
        Self { start, generation }
    }

    /// The timeline sample the play starts from.
    pub fn start(&self) -> SamplePosition {
        self.start
    }

    /// The generation of the disk streams the play reads; 0 for none.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl From<SamplePosition> for Cue {
    /// A cue that reads no disk stream.
    fn from(start: SamplePosition) -> Self {
        Self {
            start,
            generation: 0,
        }
    }
}

/// The last play started, as the capture tap places takes by it (ADR-0032).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlayInfo {
    /// The play's number, from 1.
    pub(crate) sequence: u64,
    /// The timeline sample it plays from, the audible region's start.
    pub(crate) from: i64,
    /// The engine sample at which the output plays `from`.
    pub(crate) audible_at: i64,
    /// The audible region's stop, once asked; `from` for a play stopped before anything audible was
    /// read.
    pub(crate) stop: Option<i64>,
}

/// Whether a play may start, as far as its disk streams go: it reads none, or a disk reader has
/// primed its generation.
fn is_primed(play: &Play, stats: &EngineStats) -> bool {
    play.generation == 0 || stats.primed_generation() >= play.generation
}

/// The processor's transport: its clock, the audible region, and a play that waits for a stop to
/// end or for its cue to be primed.
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
    /// A play that waits: asked while rolling, it starts at the first period after the stop ends;
    /// of a generation not yet primed, at the first period that finds it primed.
    waiting: Option<Play>,
    /// The number of the last play started; 0 before any.
    play: u64,
    /// The engine sample at which the output plays the last play's `from`.
    audible_at: i64,
    /// The generation of the disk streams the last play started reads; 0 for none.
    generation: u64,
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
            audible_at: 0,
            generation: 0,
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
            && let Some(play) = self.waiting.take_if(|play| is_primed(play, stats))
        {
            self.start(play, period_start, compensation, stats);
        }
        while let Ok(command) = self.commands.pop() {
            match command {
                TransportCommand::Play(play) => {
                    if self.rolling {
                        self.ask_to_stop(compensation);
                    }
                    if self.rolling || !is_primed(&play, stats) {
                        self.waiting = Some(play);
                    } else {
                        self.waiting = None;
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
        self.generation = play.generation;
        self.from = play.from.0;
        self.stop = None;
        self.clock = play.from.0.saturating_sub(compensation);
        self.audible_at = period_start.saturating_add(compensation);
        stats.publish_play_start(play.sequence, SamplePosition(self.audible_at));
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

    /// The last play started, whether it still rolls or not; `None` before any.
    pub(crate) fn last_play(&self) -> Option<PlayInfo> {
        (self.play > 0).then_some(PlayInfo {
            sequence: self.play,
            from: self.from,
            audible_at: self.audible_at,
            stop: self.stop,
        })
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
            generation: self.generation,
        }
    }

    /// Moves the clock on by `frames` frames, cut where the transport stops as the processor's
    /// sub-blocks are, for a processor whose graph no longer runs (ADR-0022): a stop asked after the
    /// graph's panic still ends the takes under way, the clock never jumps past the stop.
    pub(crate) fn advance_over(&mut self, frames: usize) {
        let mut left = frames;
        while left > 0 && self.rolling {
            let step = self
                .frames_until_stop()
                .map_or(left, |until| until.min(left))
                .max(1);
            self.advance(step);
            left = left.saturating_sub(step);
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
