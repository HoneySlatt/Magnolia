//! Fuzzes the WAV reader with arbitrary bytes, read as a file. A header that reads must name
//! frames that lie within the file and leave the reader at them; its frames must all decode, and
//! the samples decoded must write back through the writer to the same bytes, or for 32-bit
//! integers to the same samples. Nothing may panic.
//!
//! From an empty corpus, as in CI, raw bytes seldom grow a `fmt ` chunk within seconds. So most
//! inputs build a file shaped like a WAV file, whose every id, size and field may still be
//! arbitrary, cut short or lying; the rest are read as they are.

#![no_main]

use std::io::Cursor;

use libfuzzer_sys::arbitrary::{Result, Unstructured};
use libfuzzer_sys::fuzz_target;
use magnolia_engine::wav::{self, SampleFormat, WavInfo, WavWriter};

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

/// Builds a file of up to eight chunks: mostly the ids, sizes and fields of a WAV file, rarely
/// arbitrary ones; a pad byte sometimes missing; the whole sometimes cut short.
fn build_file(input: &mut Unstructured<'_>, file: &mut Vec<u8>) -> Result<()> {
    file.extend(pick(input, &[*b"RIFF", *b"RF64", *b"BW64"])?);
    file.extend(input.arbitrary::<u32>()?.to_le_bytes());
    file.extend(pick(input, &[*b"WAVE"])?);
    for _ in 0..input.int_in_range(0..=8)? {
        let id = pick(input, &[*b"fmt ", *b"data", *b"ds64", *b"LIST", *b"JUNK"])?;
        let body = match &id {
            b"fmt " => format_body(input)?,
            b"ds64" => ds64_body(input)?,
            _ => {
                let len = input.int_in_range(0..=512)?.min(input.len());
                input.bytes(len)?.to_vec()
            }
        };
        let size = match input.int_in_range(0..=7)? {
            0 => 0,
            1 => u32::MAX,
            2 => input.arbitrary()?,
            _ => u32::try_from(body.len()).expect("a body of at most 512 bytes"),
        };
        file.extend(id);
        file.extend(size.to_le_bytes());
        file.extend(&body);
        if body.len() % 2 == 1 && input.ratio(7, 8)? {
            file.push(0);
        }
    }
    if input.ratio(1, 8)? {
        file.truncate(input.int_in_range(0..=file.len())?);
    }
    Ok(())
}

/// One of `choices`, or rarely four arbitrary bytes.
fn pick(input: &mut Unstructured<'_>, choices: &[[u8; 4]]) -> Result<[u8; 4]> {
    if input.ratio(1, 16)? {
        input.arbitrary()
    } else {
        input.choose(choices).copied()
    }
}

/// A `fmt ` chunk's body, mostly valid, WAVE_FORMAT_EXTENSIBLE's extension included.
fn format_body(input: &mut Unstructured<'_>) -> Result<Vec<u8>> {
    let format_tag: u16 = often(input, |input| input.choose(&[1, 3, 0xFFFE]).copied())?;
    let channels: u16 = often(input, |input| input.int_in_range(1..=8))?;
    let sample_rate: u32 = often(input, |input| input.choose(&[44_100, 48_000]).copied())?;
    let bits: u16 = often(input, |input| input.choose(&[16, 24, 32]).copied())?;
    let block_align: u16 = often(input, |_| Ok(channels.wrapping_mul(bits / 8)))?;
    let mut body = [
        format_tag.to_le_bytes().as_slice(),
        &channels.to_le_bytes(),
        &sample_rate.to_le_bytes(),
        &input.arbitrary::<u32>()?.to_le_bytes(),
        &block_align.to_le_bytes(),
        &bits.to_le_bytes(),
    ]
    .concat();
    if format_tag == 0xFFFE || input.ratio(1, 8)? {
        let sub_format_tag: u16 = often(input, |input| input.choose(&[1, 3]).copied())?;
        body.extend(22_u16.to_le_bytes());
        body.extend(input.arbitrary::<[u8; 6]>()?);
        body.extend(sub_format_tag.to_le_bytes());
        body.extend([
            0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
        ]);
    }
    cut_sometimes(input, &mut body)?;
    Ok(body)
}

/// A `ds64` chunk's body: the RIFF's size, a data size mostly within a fuzzer's input, a sample
/// count and a table's length.
fn ds64_body(input: &mut Unstructured<'_>) -> Result<Vec<u8>> {
    let data_size: u64 = often(input, |input| input.int_in_range(0..=4_096))?;
    let mut body = [
        input.arbitrary::<u64>()?.to_le_bytes().as_slice(),
        &data_size.to_le_bytes(),
        &input.arbitrary::<u64>()?.to_le_bytes(),
        &input.arbitrary::<u32>()?.to_le_bytes(),
    ]
    .concat();
    cut_sometimes(input, &mut body)?;
    Ok(body)
}

/// What `likely` gives, fifteen times in sixteen; an arbitrary value otherwise.
fn often<'a, T: libfuzzer_sys::arbitrary::Arbitrary<'a>>(
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

fn check(file: &[u8]) {
    let mut reader = Cursor::new(file);
    let Ok(info) = wav::read_info(&mut reader) else {
        return;
    };
    assert_eq!(
        reader.position(),
        info.data_offset,
        "the reader is left at the data"
    );
    assert!((1..=1_024).contains(&info.channels), "{info:?}");
    assert!(info.sample_rate > 0, "{info:?}");

    let channels = usize::from(info.channels);
    let frame_bytes = channels * info.sample_format.bytes_per_sample();
    let start = usize::try_from(info.data_offset).expect("an offset within the input");
    let frames = usize::try_from(info.frames).expect("frames within the input");
    let end = frames
        .checked_mul(frame_bytes)
        .and_then(|bytes| bytes.checked_add(start))
        .filter(|&end| end <= file.len())
        .unwrap_or_else(|| panic!("the frames lie past the end of the file: {info:?}"));
    let data = &file[start..end];
    if info.length_repaired {
        assert!(
            file.len() - end < frame_bytes,
            "a repaired length reads every whole frame to the end"
        );
    }

    // Given the named bytes, decode decodes them all; given the rest of the file and room for
    // fewer frames, it fills that room with the same samples.
    let mut samples = vec![0.0; frames * channels];
    assert_eq!(
        wav::decode(info.sample_format, info.channels, data, &mut samples),
        frames
    );
    let mut fewer = vec![0.0; (frames / 2) * channels];
    assert_eq!(
        wav::decode(
            info.sample_format,
            info.channels,
            &file[start..],
            &mut fewer
        ),
        frames / 2
    );
    assert_eq!(
        fewer.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
        samples[..fewer.len()]
            .iter()
            .map(|s| s.to_bits())
            .collect::<Vec<_>>()
    );
    // A 32-bit integer near its largest rounds up to 1 in f32.
    if info.sample_format != SampleFormat::Float32 {
        assert!(
            samples.iter().all(|s| (-1.0..=1.0).contains(s)),
            "an integer decodes within [-1, 1]"
        );
    }

    check_writes_back(&info, data, &samples);
}

/// Writes the decoded samples through the writer: the 16- and 24-bit integers and the floats come
/// back as the same bytes, and the 32-bit integers, which decoding rounds to f32, as the same
/// samples.
fn check_writes_back(info: &WavInfo, data: &[u8], samples: &[f32]) {
    // The writer refuses a rate whose bytes per second overflow their 32-bit field; the reader
    // does not read that field.
    let Ok(mut writer) = WavWriter::new(
        Cursor::new(Vec::new()),
        info.sample_format,
        info.channels,
        info.sample_rate,
    ) else {
        return;
    };
    writer.write_frames(samples).expect("whole frames");
    let written = writer.finish().expect("written").into_inner();
    let mut reader = Cursor::new(written.as_slice());
    let written_info = wav::read_info(&mut reader).expect("the writer's file reads");
    assert_eq!(written_info.frames, info.frames);
    assert!(
        !written_info.length_repaired,
        "a finished file reads as finished"
    );
    let start = usize::try_from(written_info.data_offset).expect("a small offset");
    let written_data = &written[start..start + data.len()];
    if info.sample_format == SampleFormat::Int32 {
        let mut again = vec![0.0; samples.len()];
        wav::decode(info.sample_format, info.channels, written_data, &mut again);
        assert_eq!(again, samples);
    } else {
        assert_eq!(written_data, data);
    }
}
