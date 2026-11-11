//! Fuzzes the test session's parser with arbitrary text, raw or built (`session_text.rs`). Nothing
//! may panic; a session that parses must name no file outside its folder, and must write back to
//! text that parses.
//!
//! The sessions it builds are shaped like real ones, but every number, name and choice may still be
//! arbitrary or out of range, so that the parser's refusals are fuzzed as much as what it accepts.

#![no_main]

mod session_text;

use std::path::{Component, Path};

use libfuzzer_sys::arbitrary::{Result, Unstructured};
use libfuzzer_sys::fuzz_target;
use magnolia_session::file::{
    self, Bus, Clip, Insert, Instrument, Master, MeterChange, SessionFile, TempoChange,
    TempoSection, Track, TrackKind, TrackSend,
};

use session_text::{often, session_text};

/// The names outputs, sends and buses draw from, so that they meet, clash and loop.
const NAMES: [&str; 5] = ["master", "drums", "beat", "room", "vocals"];

fuzz_target!(|input: &[u8]| {
    if let Some(text) = session_text(input, session) {
        check(&text);
    }
});

fn check(text: &str) {
    let Ok(session) = file::parse(text) else {
        return;
    };
    // `../c.mid`, which the builder offers, is always refused: no path leaves the session's folder.
    for track in &session.tracks {
        for clip in &track.clips {
            let inside = Path::new(&clip.file)
                .components()
                .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
            assert!(
                inside,
                "a path that leaves the session's folder parsed: {:?}",
                clip.file
            );
        }
    }
    let written = session.to_json().expect("a parsed session writes");
    if let Err(error) = file::parse(&written) {
        panic!("a session that parsed does not parse once written: {error}\n{written}");
    }
}

fn session(input: &mut Unstructured<'_>) -> Result<SessionFile> {
    Ok(SessionFile {
        format: if input.ratio(15, 16)? {
            file::FORMAT.to_owned()
        } else {
            input.arbitrary()?
        },
        version: often(input, |_| Ok(file::VERSION))?,
        sample_rate: often(input, |input| {
            input.choose(&[44_100, 48_000, 96_000, 44_056]).copied()
        })?,
        tempo: TempoSection {
            origin: often(input, |input| input.int_in_range(0..=1 << 40))?,
            changes: list(input, 3, |input| {
                Ok(TempoChange {
                    tick: often(input, |input| input.int_in_range(0..=1 << 32))?,
                    bpm: [
                        often(input, |input| input.int_in_range(1..=1_000))?,
                        often(input, |input| input.int_in_range(1..=4))?,
                    ],
                })
            })?,
            meters: list(input, 3, |input| {
                Ok(MeterChange {
                    bar: often(input, |input| input.int_in_range(0..=64))?,
                    beats: often(input, |input| input.int_in_range(1..=16))?,
                    beat_unit: often(input, |input| input.choose(&[2, 4, 8]).copied())?,
                })
            })?,
        },
        master: Master {
            channels: often(input, |_| Ok(2))?,
            inserts: list(input, 2, insert)?,
        },
        buses: list(input, 4, |input| {
            Ok(Bus {
                name: name(input)?,
                channels: often(input, |input| input.int_in_range(1..=2))?,
                inserts: list(input, 2, insert)?,
                output: name(input)?,
                gain: gain(input)?,
            })
        })?,
        tracks: list(input, 4, track)?,
    })
}

fn track(input: &mut Unstructured<'_>) -> Result<Track> {
    let kind = *input.choose(&[TrackKind::Audio, TrackKind::Midi])?;
    let audio = kind == TrackKind::Audio;
    // Mostly the fields of its kind; sometimes the other kind's, or none.
    let fits = input.ratio(7, 8)?;
    Ok(Track {
        name: input.arbitrary()?,
        kind,
        channels: if audio == fits {
            Some(often(input, |input| input.int_in_range(1..=2))?)
        } else {
            None
        },
        instrument: (audio != fits).then_some(Instrument::TestSynth {}),
        clips: list(input, 4, |input| {
            Ok(Clip {
                file: often(input, |input| {
                    input
                        .choose(&["a.wav", "b.wav", "../c.mid"])
                        .map(|name| (*name).to_owned())
                })?,
                start: often(input, |input| input.int_in_range(0..=1 << 40))?,
                file_offset: if audio == fits {
                    Some(often(input, |input| input.int_in_range(0..=1 << 20))?)
                } else {
                    None
                },
                length: often(input, |input| input.int_in_range(1..=1 << 30))?,
                gain: if audio == fits {
                    Some(gain(input)?)
                } else {
                    None
                },
            })
        })?,
        inserts: list(input, 2, insert)?,
        output: name(input)?,
        gain: gain(input)?,
        sends: list(input, 2, |input| {
            Ok(TrackSend {
                to: name(input)?,
                gain: gain(input)?,
            })
        })?,
    })
}

fn insert(input: &mut Unstructured<'_>) -> Result<Insert> {
    Ok(if input.arbitrary()? {
        Insert::TestLatency {
            samples: often(input, |input| input.int_in_range(0..=70_000))?,
        }
    } else {
        Insert::Gain {
            value: gain(input)?,
        }
    })
}

/// A gain, mostly between -2 and 2, sometimes any `f32`, infinities and NaN included.
fn gain(input: &mut Unstructured<'_>) -> Result<f32> {
    often(input, |input| {
        Ok(f32::from(input.int_in_range(-200_i16..=200)?) / 100.0)
    })
}

fn name(input: &mut Unstructured<'_>) -> Result<String> {
    often(input, |input| {
        input.choose(&NAMES).map(|name| (*name).to_owned())
    })
}

/// Up to `most` items.
fn list<T>(
    input: &mut Unstructured<'_>,
    most: usize,
    mut item: impl FnMut(&mut Unstructured<'_>) -> Result<T>,
) -> Result<Vec<T>> {
    (0..input.int_in_range(0..=most)?)
        .map(|_| item(input))
        .collect()
}
