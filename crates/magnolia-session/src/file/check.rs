//! The checks of a session file that need no file on disk, and what they resolve, which the loader
//! builds from: the routing of buses and tracks, and each clip placed on the timeline. The loader
//! calls the same functions, so that one rule never has two copies.

use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

use magnolia_engine::node::MAX_LATENCY;
use magnolia_engine::time::{
    Meter, MusicalPosition, SamplePosition, SampleRate, Tempo, TempoMap, TempoMapError,
    TimePosition, TimeUnits, place,
};

use super::{
    Bus, Clip, FileError, INPUT_TRACKS_SINCE, Insert, Instrument, MASTER, MAX_AUDIO_FILES,
    MAX_CHANNELS, Problem, SessionFile, TempoSection, Track, TrackKind, TrackSend,
};

/// The channels of a MIDI track's instrument: the test synth's two outputs.
pub(crate) const INSTRUMENT_CHANNELS: u16 = 2;

/// The longest a test session lasts: every clip, audio or MIDI, ends within 24 hours of the
/// timeline's zero, so that a render writes at most a day of audio and its tail. At the highest
/// rate, 768 kHz, that is 6.6e10 samples, well within the disk reader's 2^41 frames.
const LONGEST_SESSION_SECONDS: i64 = 24 * 60 * 60;

/// The sample after the last a clip may hold at `rate`: 24 hours from the timeline's zero.
fn session_end(rate: SampleRate) -> i64 {
    i64::from(rate.hertz()).saturating_mul(LONGEST_SESSION_SECONDS)
}

/// Where an output or a send goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Target {
    /// The master.
    Master,
    /// A bus, by its index in the session's buses.
    Bus(usize),
}

/// A bus's or a track's chain, its destinations resolved: its inserts in series, its gain, its
/// output, and its sends after its gain.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Strip {
    pub(crate) channels: u16,
    pub(crate) inserts: Vec<Insert>,
    pub(crate) gain: f32,
    pub(crate) output: Target,
    /// Each send's destination and gain.
    pub(crate) sends: Vec<(Target, f32)>,
}

/// Every bus's and every track's strip, in the session's order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Routing {
    pub(crate) buses: Vec<Strip>,
    pub(crate) tracks: Vec<Strip>,
}

/// What a track plays, its kind's fields checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrackSource {
    /// Clips of WAV files of this many channels.
    Audio { channels: u16 },
    /// Clips of MIDI files, into this instrument.
    Midi(Instrument),
    /// The capture channels its `inputs` name, one per channel, live; recorded when armed.
    Input { channels: u16, armed: bool },
}

/// An audio clip placed on the sample grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PlacedAudioClip {
    /// The first sample at or after its start.
    pub(crate) start: SamplePosition,
    /// The sample after its last frame.
    pub(crate) end: i64,
    pub(crate) file_offset: u64,
    pub(crate) gain: f32,
}

/// A MIDI clip, its range checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlacedMidiClip {
    pub(crate) start: MusicalPosition,
    /// Its length in ticks.
    pub(crate) length: i64,
    /// The sample its end is placed on.
    pub(crate) end: SamplePosition,
}

impl SessionFile {
    /// Every check that needs no file on disk; the version and the format are checked before.
    pub(super) fn check(&self) -> Result<(), FileError> {
        let rate = self.rate()?;
        let tempo_map = self.tempo.map()?;
        self.check_kinds_in_version()?;
        self.routing()?;
        let mut audio_files = BTreeSet::new();
        // Each capture channel an input track names, by the field that names it first.
        let mut capture_channels = HashMap::new();
        for (index, track) in self.tracks.iter().enumerate() {
            let field = format!("tracks[{index}]");
            match track.source(&field)? {
                TrackSource::Audio { .. } => {
                    check_audio_clips(&field, track, rate, &mut audio_files)?;
                }
                TrackSource::Midi(_) => check_midi_clips(&field, track, rate, &tempo_map)?,
                TrackSource::Input { .. } => {
                    check_capture_channels_free(&field, track, &mut capture_channels)?;
                }
            }
        }
        Ok(())
    }

    /// Refuses a kind of track that the file's version does not have: an input track in version 1.
    fn check_kinds_in_version(&self) -> Result<(), FileError> {
        if self.version >= INPUT_TRACKS_SINCE {
            return Ok(());
        }
        match self
            .tracks
            .iter()
            .position(|track| track.kind == TrackKind::Input)
        {
            Some(index) => Err(FileError::new(
                format!("tracks[{index}].kind"),
                Problem::NotInVersion {
                    version: self.version,
                    since: INPUT_TRACKS_SINCE,
                },
            )),
            None => Ok(()),
        }
    }

    /// The capture channels the engine needs: the highest that an input track names, 0 without
    /// input tracks. Each is checked first, by [`parse`](super::parse).
    pub(crate) fn capture_channels(&self) -> u16 {
        self.tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Input)
            .flat_map(|track| track.inputs.iter().flatten())
            .copied()
            .max()
            .unwrap_or(0)
    }

    /// The session's sample rate.
    pub(crate) fn rate(&self) -> Result<SampleRate, FileError> {
        SampleRate::new(self.sample_rate).ok_or_else(|| {
            out_of_range(
                "sample_rate",
                self.sample_rate,
                "8,000 to 768,000 hertz, a sample a whole number of time units, such as 44,100 \
                 or 48,000",
            )
        })
    }

    /// Every bus's and track's strip: the master's channels and inserts checked, each bus's name,
    /// each chain's inserts and gains, each output and send resolved and the channels of what it
    /// feeds checked, and the buses free of cycles.
    pub(crate) fn routing(&self) -> Result<Routing, FileError> {
        check_channels("master.channels", self.master.channels, MASTER_CHANNELS)?;
        check_inserts("master.inserts", &self.master.inserts)?;
        let destinations = Destinations::new(self)?;
        let mut buses = Vec::with_capacity(self.buses.len());
        for (index, bus) in self.buses.iter().enumerate() {
            let field = format!("buses[{index}]");
            check_channels(&format!("{field}.channels"), bus.channels, CHANNELS)?;
            let chain = Chain {
                inserts: &bus.inserts,
                gain: bus.gain,
                output: &bus.output,
                sends: &[],
            };
            buses.push(destinations.strip(&field, bus.channels, chain)?);
        }
        check_bus_cycles(&self.buses, &buses)?;
        let mut tracks = Vec::with_capacity(self.tracks.len());
        for (index, track) in self.tracks.iter().enumerate() {
            let field = format!("tracks[{index}]");
            let channels = match track.source(&field)? {
                TrackSource::Audio { channels } | TrackSource::Input { channels, .. } => channels,
                TrackSource::Midi(_) => INSTRUMENT_CHANNELS,
            };
            let chain = Chain {
                inserts: &track.inserts,
                gain: track.gain,
                output: &track.output,
                sends: &track.sends,
            };
            tracks.push(destinations.strip(&field, channels, chain)?);
        }
        Ok(Routing { buses, tracks })
    }
}

impl TempoSection {
    /// The tempo map the section describes, each field checked; the errors name the fields as
    /// they lie in a session file, under `tempo`.
    ///
    /// # Errors
    ///
    /// A [`FileError`] for a value out of range, or changes the map refuses.
    pub fn map(&self) -> Result<TempoMap, FileError> {
        if self.origin < 0 {
            return Err(out_of_range(
                "tempo.origin",
                self.origin,
                "a time position at or after the timeline's zero",
            ));
        }
        let mut tempo_changes = Vec::with_capacity(self.changes.len());
        for (index, change) in self.changes.iter().enumerate() {
            let [numerator, denominator] = change.bpm;
            let bpm = Tempo::from_bpm(numerator, denominator).ok_or_else(|| {
                FileError::new(
                    format!("tempo.changes[{index}].bpm"),
                    Problem::OutOfRange {
                        value: format!("[{numerator}, {denominator}]"),
                        accepted: "1 to 1,000 quarter notes a minute, as [numerator, \
                                   denominator], the denominator above 0",
                    },
                )
            })?;
            tempo_changes.push((MusicalPosition(change.tick), bpm));
        }
        let mut meter_changes = Vec::with_capacity(self.meters.len());
        for (index, change) in self.meters.iter().enumerate() {
            let meter = Meter::new(change.beats, change.beat_unit).ok_or_else(|| {
                FileError::new(
                    format!("tempo.meters[{index}]"),
                    Problem::OutOfRange {
                        value: format!("{}/{}", change.beats, change.beat_unit),
                        accepted: "at least one beat, each a note of a power of two from 1 to 64",
                    },
                )
            })?;
            meter_changes.push((change.bar, meter));
        }
        TempoMap::new(TimePosition(self.origin), &tempo_changes, &meter_changes).map_err(|error| {
            let field = match error {
                TempoMapError::TempoNotFromStart | TempoMapError::TempoChangesOutOfOrder => {
                    "tempo.changes"
                }
                TempoMapError::MeterNotFromStart | TempoMapError::MeterChangesOutOfOrder => {
                    "tempo.meters"
                }
                TempoMapError::OutOfRange => "tempo",
            };
            FileError::new(field, Problem::TempoMap(error))
        })
    }
}

impl Track {
    /// What the track plays, the fields of its kind checked; `field` is its path. An input track's
    /// capture channels are checked here; that no two input tracks share one, by `parse`.
    pub(crate) fn source(&self, field: &str) -> Result<TrackSource, FileError> {
        match self.kind {
            TrackKind::Audio => {
                const KIND: &str = "an audio track";
                self.refuse_live_fields(field, KIND)?;
                if self.instrument.is_some() {
                    return Err(not_for_kind(format!("{field}.instrument"), KIND));
                }
                self.require_clips(field, KIND)?;
                let channels = self.channels(field, KIND)?;
                Ok(TrackSource::Audio { channels })
            }
            TrackKind::Midi => {
                const KIND: &str = "a MIDI track";
                self.refuse_live_fields(field, KIND)?;
                if self.channels.is_some() {
                    return Err(not_for_kind(format!("{field}.channels"), KIND));
                }
                self.require_clips(field, KIND)?;
                self.instrument
                    .map(TrackSource::Midi)
                    .ok_or_else(|| missing_for_kind(format!("{field}.instrument"), KIND))
            }
            TrackKind::Input => {
                const KIND: &str = "an input track";
                if self.instrument.is_some() {
                    return Err(not_for_kind(format!("{field}.instrument"), KIND));
                }
                if self.clips.is_some() {
                    return Err(not_for_kind(format!("{field}.clips"), KIND));
                }
                let channels = self.channels(field, KIND)?;
                let inputs = self
                    .inputs
                    .as_deref()
                    .ok_or_else(|| missing_for_kind(format!("{field}.inputs"), KIND))?;
                check_inputs(&format!("{field}.inputs"), inputs, channels)?;
                let armed = self
                    .armed
                    .ok_or_else(|| missing_for_kind(format!("{field}.armed"), KIND))?;
                Ok(TrackSource::Input { channels, armed })
            }
        }
    }

    /// Its channels, which a track of `kind` needs, checked; `field` is its path.
    fn channels(&self, field: &str, kind: &'static str) -> Result<u16, FileError> {
        let channels_field = format!("{field}.channels");
        let channels = self
            .channels
            .ok_or_else(|| missing_for_kind(channels_field.clone(), kind))?;
        check_channels(&channels_field, channels, CHANNELS)?;
        Ok(channels)
    }

    /// Refuses an input track's fields on a track of another `kind`.
    fn refuse_live_fields(&self, field: &str, kind: &'static str) -> Result<(), FileError> {
        if self.inputs.is_some() {
            return Err(not_for_kind(format!("{field}.inputs"), kind));
        }
        if self.armed.is_some() {
            return Err(not_for_kind(format!("{field}.armed"), kind));
        }
        Ok(())
    }

    /// Refuses a track of `kind` that gives no clips: an empty list says it has none.
    fn require_clips(&self, field: &str, kind: &'static str) -> Result<(), FileError> {
        match self.clips {
            Some(_) => Ok(()),
            None => Err(missing_for_kind(format!("{field}.clips"), kind)),
        }
    }
}

/// Checks an input track's capture channels, given at `field`: each from 1 to [`MAX_CHANNELS`],
/// one per channel of the track.
fn check_inputs(field: &str, inputs: &[u16], channels: u16) -> Result<(), FileError> {
    for (index, &input) in inputs.iter().enumerate() {
        if !(1..=MAX_CHANNELS).contains(&input) {
            return Err(out_of_range(
                format!("{field}[{index}]"),
                input,
                "a capture channel from 1 to 1,024",
            ));
        }
    }
    if inputs.len() != usize::from(channels) {
        return Err(FileError::new(
            field,
            Problem::InputCount {
                inputs: inputs.len(),
                channels,
            },
        ));
    }
    Ok(())
}

/// Refuses an input track, at `field`, that names a capture channel an earlier input names, in
/// this track or another: `taken` holds each channel named so far, by the field that names it.
fn check_capture_channels_free(
    field: &str,
    track: &Track,
    taken: &mut HashMap<u16, String>,
) -> Result<(), FileError> {
    for (index, &channel) in track.inputs.iter().flatten().enumerate() {
        let input_field = format!("{field}.inputs[{index}]");
        if let Some(by) = taken.get(&channel) {
            return Err(FileError::new(
                input_field,
                Problem::CaptureChannelTaken {
                    channel,
                    by: by.clone(),
                },
            ));
        }
        taken.insert(channel, input_field);
    }
    Ok(())
}

/// The channels a bus or a track may have, and what the field accepts.
const CHANNELS: (u16, &str) = (1, "1 to 1,024 channels");

/// The channels the master may have: the playback channels, at least two.
const MASTER_CHANNELS: (u16, &str) = (2, "2 to 1,024 channels");

/// A strip's chain, as the file gives it.
struct Chain<'a> {
    inserts: &'a [Insert],
    gain: f32,
    output: &'a str,
    sends: &'a [TrackSend],
}

/// What outputs and sends may name: the master, and the buses by name.
struct Destinations<'a> {
    master_channels: u16,
    /// Each bus's index, by its name.
    buses: HashMap<&'a str, usize>,
    /// Each bus's channels, in order.
    bus_channels: Vec<u16>,
}

impl<'a> Destinations<'a> {
    /// The session's destinations, each bus's name checked: not the master's, not an earlier
    /// bus's.
    fn new(file: &'a SessionFile) -> Result<Self, FileError> {
        let mut buses = HashMap::with_capacity(file.buses.len());
        for (index, bus) in file.buses.iter().enumerate() {
            if bus.name == MASTER || buses.insert(bus.name.as_str(), index).is_some() {
                return Err(FileError::new(
                    format!("buses[{index}].name"),
                    Problem::NameTaken(bus.name.clone()),
                ));
            }
        }
        Ok(Self {
            master_channels: file.master.channels,
            buses,
            bus_channels: file.buses.iter().map(|bus| bus.channels).collect(),
        })
    }

    /// A strip of `channels` channels, its chain checked and its destinations resolved; `field`
    /// is the path of its bus or track.
    fn strip(&self, field: &str, channels: u16, chain: Chain<'_>) -> Result<Strip, FileError> {
        check_inserts(&format!("{field}.inserts"), chain.inserts)?;
        check_gain(&format!("{field}.gain"), chain.gain)?;
        let output = self.feed(&format!("{field}.output"), channels, chain.output)?;
        let mut sends = Vec::with_capacity(chain.sends.len());
        for (index, send) in chain.sends.iter().enumerate() {
            let send_field = format!("{field}.sends[{index}]");
            let to = self.feed(&format!("{send_field}.to"), channels, &send.to)?;
            check_gain(&format!("{send_field}.gain"), send.gain)?;
            sends.push((to, send.gain));
        }
        Ok(Strip {
            channels,
            inserts: chain.inserts.to_vec(),
            gain: chain.gain,
            output,
            sends,
        })
    }

    /// Where `name`, given at `field`, goes, checked to take `channels` channels: as many, or two
    /// for one.
    fn feed(&self, field: &str, channels: u16, name: &str) -> Result<Target, FileError> {
        let (target, to) = if name == MASTER {
            (Target::Master, self.master_channels)
        } else {
            let index = self.buses.get(name).copied().ok_or_else(|| {
                FileError::new(field, Problem::UnknownDestination(name.to_owned()))
            })?;
            let to = self.bus_channels.get(index).copied().unwrap_or_default();
            (Target::Bus(index), to)
        };
        if channels != to && !(channels == 1 && to == 2) {
            return Err(FileError::new(
                field,
                Problem::ChannelMismatch { from: channels, to },
            ));
        }
        Ok(target)
    }
}

/// Refuses a bus whose output leads back to it. Each bus has one output, so the buses and their
/// outputs form chains that end in the master unless they loop; each bus is walked once.
fn check_bus_cycles(buses: &[Bus], strips: &[Strip]) -> Result<(), FileError> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Visit {
        New,
        OnPath,
        Done,
    }
    let mut visits = vec![Visit::New; strips.len()];
    let mut path: Vec<usize> = Vec::new();
    for start in 0..strips.len() {
        path.clear();
        let mut next = Some(start);
        while let Some(bus) = next {
            match visits.get(bus) {
                Some(Visit::New) => {}
                Some(Visit::OnPath) => {
                    let from = path.iter().position(|&on| on == bus).unwrap_or_default();
                    let cycle = path
                        .get(from..)
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|&on| buses.get(on))
                        .map(|on| on.name.clone())
                        .collect();
                    let closing = path.last().copied().unwrap_or(bus);
                    return Err(FileError::new(
                        format!("buses[{closing}].output"),
                        Problem::BusCycle(cycle),
                    ));
                }
                Some(Visit::Done) | None => break,
            }
            if let Some(visit) = visits.get_mut(bus) {
                *visit = Visit::OnPath;
            }
            path.push(bus);
            next = match strips.get(bus).map(|strip| strip.output) {
                Some(Target::Bus(output)) => Some(output),
                _ => None,
            };
        }
        for &bus in &path {
            if let Some(visit) = visits.get_mut(bus) {
                *visit = Visit::Done;
            }
        }
    }
    Ok(())
}

/// Checks an audio track's clips: each placed, its file counted among the session's, and no two
/// overlapping. `field` is the track's path.
fn check_audio_clips(
    field: &str,
    track: &Track,
    rate: SampleRate,
    audio_files: &mut BTreeSet<PathBuf>,
) -> Result<(), FileError> {
    let mut spans = Vec::with_capacity(track.clips().len());
    for (index, clip) in track.clips().iter().enumerate() {
        let clip_field = format!("{field}.clips[{index}]");
        let file_field = format!("{clip_field}.file");
        let path = relative_path(&file_field, &clip.file)?;
        if audio_files.insert(path) && audio_files.len() > MAX_AUDIO_FILES {
            return Err(FileError::new(file_field, Problem::TooManyAudioFiles));
        }
        let placed = place_audio_clip(&clip_field, clip, rate)?;
        spans.push((placed.start.0, placed.end, index));
    }
    // By start, then end: a clip overlaps the one before it exactly when it starts before that
    // one ends, as every clip lasts at least a frame.
    spans.sort_unstable();
    for pair in spans.windows(2) {
        if let [(_, earlier_end, earlier), (later_start, _, later)] = *pair
            && earlier_end > later_start
        {
            return Err(FileError::new(
                format!("{field}.clips[{later}].start"),
                Problem::ClipsOverlap { earlier },
            ));
        }
    }
    Ok(())
}

/// Checks a MIDI track's clips: each path, and each placed. `field` is the track's path.
fn check_midi_clips(
    field: &str,
    track: &Track,
    rate: SampleRate,
    tempo_map: &TempoMap,
) -> Result<(), FileError> {
    for (index, clip) in track.clips().iter().enumerate() {
        let clip_field = format!("{field}.clips[{index}]");
        relative_path(&format!("{clip_field}.file"), &clip.file)?;
        place_midi_clip(&clip_field, clip, rate, tempo_map)?;
    }
    Ok(())
}

/// An audio clip placed on the sample grid at `rate`, its fields checked; `field` is its path.
pub(crate) fn place_audio_clip(
    field: &str,
    clip: &Clip,
    rate: SampleRate,
) -> Result<PlacedAudioClip, FileError> {
    const KIND: &str = "an audio clip";
    let file_offset = clip
        .file_offset
        .ok_or_else(|| missing_for_kind(format!("{field}.file_offset"), KIND))?;
    let gain_field = format!("{field}.gain");
    let gain = clip
        .gain
        .ok_or_else(|| missing_for_kind(gain_field.clone(), KIND))?;
    check_gain(&gain_field, gain)?;
    check_length(field, clip.length)?;
    let start = (clip.start >= 0)
        .then(|| place(TimePosition(clip.start), &TimeUnits, &rate))
        .flatten()
        .ok_or_else(|| {
            out_of_range(
                format!("{field}.start"),
                clip.start,
                "a time position at or after the timeline's zero",
            )
        })?;
    let end = i64::try_from(clip.length)
        .ok()
        .and_then(|length| start.0.checked_add(length))
        .filter(|&end| end <= session_end(rate))
        .ok_or_else(|| {
            out_of_range(
                format!("{field}.length"),
                clip.length,
                "the clip must end within 24 hours of the timeline's zero, at the session's rate",
            )
        })?;
    if file_offset.checked_add(clip.length).is_none() {
        return Err(out_of_range(
            format!("{field}.file_offset"),
            file_offset,
            "the clip's last frame must lie within 2^64 frames of its file's start",
        ));
    }
    Ok(PlacedAudioClip {
        start,
        end,
        file_offset,
        gain,
    })
}

/// A MIDI clip, its fields checked and its end placed on the sample grid at `rate` by
/// `tempo_map`; `field` is its path.
pub(crate) fn place_midi_clip(
    field: &str,
    clip: &Clip,
    rate: SampleRate,
    tempo_map: &TempoMap,
) -> Result<PlacedMidiClip, FileError> {
    const KIND: &str = "a MIDI clip";
    if clip.file_offset.is_some() {
        return Err(not_for_kind(format!("{field}.file_offset"), KIND));
    }
    if clip.gain.is_some() {
        return Err(not_for_kind(format!("{field}.gain"), KIND));
    }
    check_length(field, clip.length)?;
    if clip.start < 0 {
        return Err(out_of_range(
            format!("{field}.start"),
            clip.start,
            "a tick at or after tick 0",
        ));
    }
    let placed = i64::try_from(clip.length).ok().and_then(|length| {
        let end = place(
            MusicalPosition(clip.start.checked_add(length)?),
            tempo_map,
            &rate,
        )?;
        (end.0 <= session_end(rate)).then_some((length, end))
    });
    let (length, end) = placed.ok_or_else(|| {
        out_of_range(
            format!("{field}.length"),
            clip.length,
            "the clip must end within 24 hours of the timeline's zero, at the session's rate",
        )
    })?;
    Ok(PlacedMidiClip {
        start: MusicalPosition(clip.start),
        length,
        end,
    })
}

/// A file's path, given at `field`, without its `.` components and repeated separators: the key by
/// which a session reads each file once. It must be relative, not empty, and stay inside the
/// session file's folder: no `..`. A symbolic link inside the folder is followed wherever it leads.
pub(crate) fn relative_path(field: &str, path: &str) -> Result<PathBuf, FileError> {
    if path.is_empty() {
        return Err(FileError::new(field, Problem::EmptyPath));
    }
    let mut relative = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                return Err(FileError::new(field, Problem::AbsolutePath));
            }
            Component::ParentDir => {
                return Err(FileError::new(field, Problem::PathLeavesFolder));
            }
            Component::CurDir => {}
            Component::Normal(_) => relative.push(component),
        }
    }
    Ok(relative)
}

fn check_channels(
    field: &str,
    channels: u16,
    (least, accepted): (u16, &'static str),
) -> Result<(), FileError> {
    if (least..=MAX_CHANNELS).contains(&channels) {
        Ok(())
    } else {
        Err(out_of_range(field, channels, accepted))
    }
}

fn check_inserts(field: &str, inserts: &[Insert]) -> Result<(), FileError> {
    for (index, insert) in inserts.iter().enumerate() {
        match *insert {
            Insert::TestLatency { samples } if samples > MAX_LATENCY => {
                return Err(out_of_range(
                    format!("{field}[{index}].test_latency.samples"),
                    samples,
                    "0 to 65,536 samples, the engine's largest latency",
                ));
            }
            Insert::Gain { value } => {
                check_gain(&format!("{field}[{index}].gain.value"), value)?;
            }
            Insert::TestLatency { .. } => {}
        }
    }
    Ok(())
}

fn check_gain(field: &str, gain: f32) -> Result<(), FileError> {
    if gain.is_finite() {
        Ok(())
    } else {
        Err(out_of_range(field, gain, "a finite number"))
    }
}

/// A clip lasts at least one frame or tick; `field` is the clip's path.
fn check_length(field: &str, length: u64) -> Result<(), FileError> {
    if length == 0 {
        Err(out_of_range(
            format!("{field}.length"),
            length,
            "at least 1",
        ))
    } else {
        Ok(())
    }
}

fn out_of_range(
    field: impl Into<String>,
    value: impl ToString,
    accepted: &'static str,
) -> FileError {
    FileError::new(
        field,
        Problem::OutOfRange {
            value: value.to_string(),
            accepted,
        },
    )
}

fn missing_for_kind(field: String, kind: &'static str) -> FileError {
    FileError::new(field, Problem::MissingForKind { kind })
}

fn not_for_kind(field: String, kind: &'static str) -> FileError {
    FileError::new(field, Problem::NotForKind { kind })
}
