//! Playing a session through a backend: [`SessionPlayer`], for the command line.

use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use magnolia_engine::callback_times::Snapshot;
use magnolia_engine::disk::{DiskReader, PrimeError};
use magnolia_engine::engine::{Controller, Processor, TransportError};
use magnolia_engine::stats::EngineStats;
use magnolia_engine::time::SamplePosition;

use crate::build::{BuildError, BuiltSession, ReaderMode};
use crate::load::{LoadedSource, Session};

/// Plays a session through whichever backend plays its processor: the JACK backend, or the
/// device-less backend in tests. Its disk reader runs on a thread of its own, woken by the
/// players.
///
/// [`new`](Self::new) builds the session and hands back the processor; the backend calls it once
/// a period. [`start`](Self::start) plays from a sample, [`stop`](Self::stop) stops, and
/// [`report`](Self::report) says what the engine and the reader have done, at any time.
pub struct SessionPlayer {
    controller: Controller,
    reader: DiskReader,
    last_sample: SamplePosition,
    /// The audio tracks' names, in the reader's order.
    audio_tracks: Vec<String>,
}

impl SessionPlayer {
    /// Builds `session` with its disk reader's thread, and returns the player and the engine's
    /// processor, for a backend to play.
    ///
    /// # Errors
    ///
    /// What [`Session::build`] refuses.
    pub fn new(session: &Session) -> Result<(Self, Processor), BuildError> {
        let BuiltSession {
            controller,
            processor,
            reader,
            last_sample,
        } = session.build(ReaderMode::Thread)?;
        let audio_tracks = session
            .tracks
            .iter()
            .filter(|track| matches!(track.source, LoadedSource::Audio(_)))
            .map(|track| track.name.clone())
            .collect();
        let player = Self {
            controller,
            reader,
            last_sample,
            audio_tracks,
        };
        Ok((player, processor))
    }

    /// Plays from timeline sample `from`: stops any play under way, then, in the order the disk
    /// reader documents, locates the reader there, waits until it has primed the cue, at most
    /// `timeout`, and sends the play, which the processor applies at its next period. Returns the
    /// play's sequence number, by which [`EngineStats::play_start`] says where its first sample
    /// reached the output.
    ///
    /// A rolling play keeps the slots of everything it may still read, which the new cue's blocks
    /// may need; its stop frees them once it reaches the players, at the backend's next period. So
    /// while a play rolls, the backend must be playing for the wait to end.
    ///
    /// # Errors
    ///
    /// [`StartError::Prime`] when the reader did not prime in time, or has stopped;
    /// [`StartError::Transport`] when the transport's queue is full.
    pub fn start(&mut self, from: SamplePosition, timeout: Duration) -> Result<u64, StartError> {
        // Stopped, a stop does nothing.
        self.controller.stop()?;
        let cue = self.reader.locate(from);
        self.reader.wait_primed(cue, timeout)?;
        Ok(self.controller.play(cue)?)
    }

    /// Stops the play: every path ends on one timeline sample, after a fade of 5 ms.
    ///
    /// # Errors
    ///
    /// [`TransportError::QueueFull`] when the transport's queue is full: stop again after a
    /// period.
    pub fn stop(&mut self) -> Result<(), TransportError> {
        self.controller.stop()
    }

    /// The sample after the session's last clip.
    pub fn last_sample(&self) -> SamplePosition {
        self.last_sample
    }

    /// The engine's statistics.
    pub fn stats(&self) -> &Arc<EngineStats> {
        self.controller.stats()
    }

    /// The disk reader, for its statistics, and for a test to wait until it has filled.
    pub fn reader(&self) -> &DiskReader {
        &self.reader
    }

    /// What the engine and the disk reader have done so far.
    pub fn report(&self) -> PlayReport {
        let stats = self.controller.stats();
        let disk = self.reader.stats();
        PlayReport {
            periods: stats.periods(),
            callback_times: stats.callback_times.snapshot(),
            disk_underruns: stats.disk_underruns(),
            events_dropped: stats.events_dropped(),
            node_errors: stats.node_errors(),
            non_finite_samples: stats.non_finite_samples(),
            own_overruns: stats.own_overruns(),
            server_xruns: stats.server_xruns(),
            refills: disk.refills(),
            longest_refill: disk.longest_refill(),
            refill_latency: disk.refill_latency(),
            bytes_read: disk.bytes_read(),
            unread_frames: disk.unread_frames(),
            read_errors: disk.read_errors(),
            low_water: self
                .audio_tracks
                .iter()
                .enumerate()
                .map(|(index, name)| (name.clone(), self.reader.low_water(index)))
                .collect(),
        }
    }
}

/// What a [`SessionPlayer`]'s engine and disk reader have done, read at one moment; its
/// [`Display`](fmt::Display) prints it for a person.
#[derive(Clone, Debug)]
pub struct PlayReport {
    /// The periods the processor has run.
    pub periods: u64,
    /// How long each period's work took.
    pub callback_times: Snapshot,
    /// The frames the audio players lacked, which played as silence.
    pub disk_underruns: u64,
    /// The events dropped, past what a node may receive in one sub-block.
    pub events_dropped: u64,
    /// How many times a node returned an error.
    pub node_errors: u64,
    /// The samples bound for the device that were not finite, replaced by zero.
    pub non_finite_samples: u64,
    /// The periods the processor finished late, as the backend counts them.
    pub own_overruns: u64,
    /// The xruns the audio server reported.
    pub server_xruns: u64,
    /// The disk reader's passes that filled something.
    pub refills: u64,
    /// The longest of them.
    pub longest_refill: Duration,
    /// From a player's ask to the end of the reader's first run after it.
    pub refill_latency: Snapshot,
    /// The bytes the reader read.
    pub bytes_read: u64,
    /// The frames of clips their files did not give, read as silence.
    pub unread_frames: u64,
    /// The reads that failed.
    pub read_errors: u64,
    /// Each audio track's name and its low-water mark: the fewest frames its player found ready
    /// ahead of it since its play started, short of the track's end; `None` while its stream has
    /// held the track to its end, or before it played. A play starts once the first half second of
    /// each stream is ready, so even a track shorter than the read-ahead may show a mark.
    pub low_water: Vec<(String, Option<u64>)>,
}

impl fmt::Display for PlayReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let quantile = |snapshot: &Snapshot, numerator, denominator| {
            snapshot
                .quantile(numerator, denominator)
                .map_or_else(|| "none".to_owned(), |time| format!("{time:?}"))
        };
        writeln!(formatter, "periods: {}", self.periods)?;
        writeln!(
            formatter,
            "callback: median {}, 99.9 % {}, longest {:?}",
            quantile(&self.callback_times, 1, 2),
            quantile(&self.callback_times, 999, 1_000),
            self.callback_times.maximum()
        )?;
        writeln!(
            formatter,
            "disk underruns: {} frames; events dropped: {}; node errors: {}; non-finite samples: \
             {}",
            self.disk_underruns, self.events_dropped, self.node_errors, self.non_finite_samples
        )?;
        writeln!(
            formatter,
            "own overruns: {}; server xruns: {}",
            self.own_overruns, self.server_xruns
        )?;
        writeln!(
            formatter,
            "disk: {} refills, the longest {:?}; refill latency median {}, longest {:?}; {} bytes \
             read; {} frames unread; {} read errors",
            self.refills,
            self.longest_refill,
            quantile(&self.refill_latency, 1, 2),
            self.refill_latency.maximum(),
            self.bytes_read,
            self.unread_frames,
            self.read_errors
        )?;
        for (name, low_water) in &self.low_water {
            match low_water {
                Some(frames) => writeln!(formatter, "low-water mark of {name:?}: {frames} frames")?,
                None => writeln!(
                    formatter,
                    "low-water mark of {name:?}: none, as its stream has held the track to its \
                     end, or it has not played"
                )?,
            }
        }
        Ok(())
    }
}

/// Why a play did not start.
#[derive(Debug)]
pub enum StartError {
    /// The disk reader did not prime the cue.
    Prime(PrimeError),
    /// The transport refused the play.
    Transport(TransportError),
}

impl From<PrimeError> for StartError {
    fn from(error: PrimeError) -> Self {
        Self::Prime(error)
    }
}

impl From<TransportError> for StartError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}

impl fmt::Display for StartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prime(error) => write!(formatter, "the disk reader did not prime: {error}"),
            Self::Transport(error) => write!(formatter, "the play was refused: {error}"),
        }
    }
}

impl Error for StartError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Prime(error) => Some(error),
            Self::Transport(error) => Some(error),
        }
    }
}
