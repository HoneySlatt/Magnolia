//! Fuzzes the WAV reader with arbitrary bytes, read as a file. A header that reads must name
//! frames that lie within the file and leave the reader at them; its frames must all decode, and
//! the samples decoded must write back through the writer to the same bytes, or for 32-bit
//! integers to the same samples, its `bext` marks with them. Nothing may panic.
//!
//! Most inputs build a file shaped like a WAV file (`wav_file`); the rest are read as they are.

#![no_main]

mod wav_file;

use std::io::Cursor;

use libfuzzer_sys::arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;
use magnolia_engine::wav::{self, Bext, BextInfo, SampleFormat, WavInfo, WavWriter};

fuzz_target!(|input: &[u8]| {
    match input.split_first() {
        // One input in eight, by its first byte, is the file itself.
        Some((&mode, raw)) if mode < 32 => check(raw),
        _ => {
            let mut file = Vec::new();
            // An input that runs out ends its file early; that is not a failure.
            let _ = wav_file::build_file(&mut Unstructured::new(input), &mut file);
            check(&file);
        }
    }
});

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
    if let Some(bext) = &info.bext {
        for text in [bext.originator(), bext.originator_reference()] {
            assert!(text.len() <= 32 && !text.contains(&0), "{bext:?}");
        }
    }

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
/// samples. A `bext` chunk whose marks are ASCII comes back with the same marks.
fn check_writes_back(info: &WavInfo, data: &[u8], samples: &[f32]) {
    let bext = info.bext.as_ref().and_then(ascii_marks);
    let file = Cursor::new(Vec::new());
    let (format, channels, rate) = (info.sample_format, info.channels, info.sample_rate);
    // The writer refuses a rate whose bytes per second overflow their 32-bit field; the reader
    // does not read that field.
    let writer = match &bext {
        Some(bext) => WavWriter::with_bext(file, format, channels, rate, bext),
        None => WavWriter::new(file, format, channels, rate),
    };
    let Ok(mut writer) = writer else {
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
    if bext.is_some() {
        let marks = |bext: &BextInfo| {
            (
                bext.time_reference,
                bext.originator().to_vec(),
                bext.originator_reference().to_vec(),
            )
        };
        assert_eq!(
            written_info.bext.as_ref().map(marks),
            info.bext.as_ref().map(marks)
        );
    }
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

/// The marks of `bext` as a `bext` chunk to write, when its texts are ASCII, as a writer takes
/// them.
fn ascii_marks(bext: &BextInfo) -> Option<Bext<'_>> {
    Some(Bext {
        originator: std::str::from_utf8(bext.originator())
            .ok()
            .filter(|text| text.is_ascii())?,
        originator_reference: std::str::from_utf8(bext.originator_reference())
            .ok()
            .filter(|text| text.is_ascii())?,
        time_reference: bext.time_reference,
        ..Bext::default()
    })
}
