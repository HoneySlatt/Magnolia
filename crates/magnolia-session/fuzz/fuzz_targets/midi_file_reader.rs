//! Fuzzes the MIDI file reader with arbitrary bytes, read as a file. A file that reads must hold
//! what its header says and what the reader promises: ticks sorted and within a file's bounds,
//! every field in its range. It must write back to bytes that read as the same file. Nothing may
//! panic.
//!
//! From an empty corpus, as in CI, raw bytes seldom grow an `MThd` header and an `MTrk` chunk of
//! valid events within seconds. So most inputs build a file shaped like a MIDI file, whose every
//! id, length, status and data byte may still be arbitrary, cut short or lying; the rest are read
//! as they are.

#![no_main]

use libfuzzer_sys::arbitrary::{Arbitrary, Result, Unstructured};
use libfuzzer_sys::fuzz_target;
use magnolia_session::midi_file::{self, EventKind, Format};

/// The largest delta time, the largest 4-byte variable-length quantity.
const MAX_DELTA: u32 = 0x0FFF_FFFF;

fuzz_target!(|input: &[u8]| {
    match input.split_first() {
        // One input in eight, by its first byte, is the file itself.
        Some((&mode, raw)) if mode < 32 => check(raw),
        _ => {
            let mut file = Vec::new();
            // An input that runs out ends its file early; that is not a failure.
            let _ = build_file(&mut Unstructured::new(input), &mut file);
            check(&file);
        }
    }
});

/// Builds a file: a header, then up to six chunks, mostly tracks of up to 64 events; the whole
/// sometimes cut short.
fn build_file(input: &mut Unstructured<'_>, file: &mut Vec<u8>) -> Result<()> {
    file.extend(pick(input, &[*b"MThd"])?);
    let format: u16 = often(input, |input| input.choose(&[0, 1]).copied())?;
    let track_count: u16 = often(input, |input| input.int_in_range(0..=4))?;
    let division: u16 = often(input, |input| input.int_in_range(1..=0x7FFF))?;
    let mut header = [format, track_count, division]
        .map(u16::to_be_bytes)
        .concat();
    if input.ratio(1, 8)? {
        let extra = input.int_in_range(0..=8)?;
        header.extend(input.bytes(extra)?);
    }
    cut_sometimes(input, &mut header)?;
    push_chunk(input, file, &header)?;

    for _ in 0..input.int_in_range(0..=6)? {
        let id = pick(input, &[*b"MTrk", *b"MTrk", *b"MTrk", *b"XFIH"])?;
        file.extend(id);
        let body = if &id == b"MTrk" {
            track_body(input)?
        } else {
            let len = input.int_in_range(0..=64)?;
            input.bytes(len)?.to_vec()
        };
        push_chunk(input, file, &body)?;
    }
    if input.ratio(1, 8)? {
        file.truncate(input.int_in_range(0..=file.len())?);
    }
    Ok(())
}

/// Appends a chunk's length, mostly its body's, then its body.
fn push_chunk(input: &mut Unstructured<'_>, file: &mut Vec<u8>, body: &[u8]) -> Result<()> {
    let real = u32::try_from(body.len()).expect("a body of a few hundred bytes");
    let length = match input.int_in_range(0..=15)? {
        0 => input.arbitrary()?,
        1 => real.saturating_sub(input.int_in_range(1..=4)?),
        2 => real.saturating_add(input.int_in_range(1..=4)?),
        _ => real,
    };
    file.extend(length.to_be_bytes());
    file.extend(body);
    Ok(())
}

/// A track's body: up to 64 events, each after its delta time, mostly ended by an end of track.
fn track_body(input: &mut Unstructured<'_>) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    for _ in 0..input.int_in_range(0..=64)? {
        delta_time(input, &mut body)?;
        event(input, &mut body)?;
    }
    if input.ratio(7, 8)? {
        body.extend([0x00, 0xFF, 0x2F, 0x00]);
    }
    Ok(body)
}

/// A delta time: mostly short, sometimes the largest, which grows gaps the writer must bridge;
/// rarely raw bytes, as a quantity of 5 bytes or cut short.
fn delta_time(input: &mut Unstructured<'_>, body: &mut Vec<u8>) -> Result<()> {
    let delta = match input.int_in_range(0..=15)? {
        0 | 1 => MAX_DELTA,
        2 => input.int_in_range(0..=MAX_DELTA)?,
        3 => {
            let len = input.int_in_range(1..=5)?;
            body.extend(input.bytes(len)?);
            return Ok(());
        }
        _ => input.int_in_range(0..=200)?,
    };
    push_variable_length(body, delta);
    Ok(())
}

/// An event: mostly a channel message, its status sometimes left to running status; else a meta
/// event, a system-exclusive event, or an arbitrary status byte.
fn event(input: &mut Unstructured<'_>, body: &mut Vec<u8>) -> Result<()> {
    match input.int_in_range(0..=15)? {
        0..=2 => {
            let meta_type = often(input, |input| {
                input.choose(&[0x51, 0x58, 0x2F, 0x01, 0x03]).copied()
            })?;
            let len = match meta_type {
                0x51 => often(input, |_| Ok(3_u8))?,
                0x58 => often(input, |_| Ok(4_u8))?,
                _ => input.int_in_range(0..=8)?,
            };
            body.extend([0xFF, meta_type]);
            push_variable_length(body, u32::from(len));
            body.extend(input.bytes(usize::from(len))?);
        }
        3 => {
            body.push(*input.choose(&[0xF0, 0xF7])?);
            let len = input.int_in_range(0..=8)?;
            push_variable_length(body, len);
            body.extend(input.bytes(usize::try_from(len).expect("a small length"))?);
        }
        4 => body.push(input.arbitrary()?),
        _ => {
            let message = input.choose(&[0x80, 0x90, 0xA0, 0xB0, 0xC0, 0xD0, 0xE0])?;
            let channel = input.int_in_range(0..=15)?;
            // Left out one time in three: the running status, if any, gives it.
            if !input.ratio(1, 3)? {
                body.push(message | channel);
            }
            let data_bytes = if matches!(message, 0xC0 | 0xD0) { 1 } else { 2 };
            for _ in 0..data_bytes {
                body.push(often(input, |input| input.int_in_range(0..=0x7F))?);
            }
        }
    }
    Ok(())
}

fn push_variable_length(body: &mut Vec<u8>, value: u32) {
    let mut started = false;
    for shift in [21, 14, 7] {
        let group = (value >> shift & 0x7F) as u8;
        if group != 0 || started {
            body.push(0x80 | group);
            started = true;
        }
    }
    body.push((value & 0x7F) as u8);
}

/// One of `choices`, or rarely four arbitrary bytes.
fn pick(input: &mut Unstructured<'_>, choices: &[[u8; 4]]) -> Result<[u8; 4]> {
    if input.ratio(1, 16)? {
        input.arbitrary()
    } else {
        input.choose(choices).copied()
    }
}

/// What `likely` gives, fifteen times in sixteen; an arbitrary value otherwise.
fn often<'a, T: Arbitrary<'a>>(
    input: &mut Unstructured<'a>,
    likely: impl FnOnce(&mut Unstructured<'a>) -> Result<T>,
) -> Result<T> {
    if input.ratio(1, 16)? {
        input.arbitrary()
    } else {
        likely(input)
    }
}

fn cut_sometimes(input: &mut Unstructured<'_>, body: &mut Vec<u8>) -> Result<()> {
    if input.ratio(1, 16)? {
        body.truncate(input.int_in_range(0..=body.len())?);
    }
    Ok(())
}

fn check(bytes: &[u8]) {
    let Ok(file) = midi_file::read(bytes) else {
        return;
    };
    // The header's six fields, after its id and length.
    let field = |at: usize| u16::from_be_bytes([bytes[at], bytes[at + 1]]);
    let format = match file.format {
        Format::SingleTrack => 0,
        Format::Parallel => 1,
    };
    assert_eq!(field(8), format, "the header's format");
    assert_eq!(
        usize::from(field(10)),
        file.tracks.len(),
        "as many tracks as the header counts"
    );
    assert_eq!(field(12), file.ticks_per_quarter, "the header's division");
    assert!((1..=0x7FFF).contains(&file.ticks_per_quarter));

    let mut events = 0;
    for track in &file.tracks {
        events += track.events.len();
        assert!(
            track.events.is_sorted_by_key(|event| event.tick),
            "the events are sorted by tick"
        );
        for event in &track.events {
            assert!(event.tick < 1 << 52, "{event:?} lies past a file's bounds");
            check_ranges(event.kind);
        }
    }
    assert!(events <= 1 << 24, "at most 16,777,216 events");

    let written = midi_file::write(&file);
    assert_eq!(
        midi_file::read(&written).as_ref(),
        Ok(&file),
        "a written file reads back as it was"
    );
}

/// Every field of an event lies in its range.
fn check_ranges(kind: EventKind) {
    let data = 0..=0x7F;
    match kind {
        EventKind::NoteOn {
            channel,
            key,
            velocity,
        } => {
            assert!(channel <= 15 && data.contains(&key), "{kind:?}");
            assert!((1..=0x7F).contains(&velocity), "{kind:?}");
        }
        EventKind::NoteOff {
            channel,
            key,
            velocity,
        } => assert!(
            channel <= 15 && data.contains(&key) && data.contains(&velocity),
            "{kind:?}"
        ),
        EventKind::Controller {
            channel,
            number,
            value,
        } => assert!(
            channel <= 15 && data.contains(&number) && data.contains(&value),
            "{kind:?}"
        ),
        EventKind::PitchBend { channel, value } => assert!(
            channel <= 15 && (-8_192..=8_191).contains(&value),
            "{kind:?}"
        ),
        EventKind::Tempo {
            microseconds_per_quarter,
        } => assert!(microseconds_per_quarter <= 0x00FF_FFFF, "{kind:?}"),
        EventKind::TimeSignature { .. } => {}
    }
}
