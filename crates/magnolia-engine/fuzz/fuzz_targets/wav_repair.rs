//! Fuzzes the repair of WAV files with arbitrary bytes, as a file in memory. A file it refuses or
//! leaves must be left byte for byte as it was. A file it repairs must read as finished, its whole
//! frames kept, exact, and a second repair must change no byte. A repair killed after any of its
//! writes, cuts and syncs must leave a file that a second repair takes to the same bytes. Nothing
//! may panic.
//!
//! An input's first byte chooses its file, and how many operations the killed repair may make.
//! One input in eight is the file itself; three in eight build a file shaped like a WAV file
//! (`wav_file`); half are written by Magnolia's writer, then hurt as a crash or a power cut may.

#![no_main]

mod wav_file;

use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};

use libfuzzer_sys::arbitrary::{Result, Unstructured};
use libfuzzer_sys::fuzz_target;
use magnolia_engine::wav::{
    self, Bext, Repair, RepairableFile, SampleFormat, WavError, WavInfo, WavWriter,
};

/// The most operations a repair makes: a write that marks the file unfinished, a cut, the pad
/// byte, a sync, two sizes, a sync.
const MOST_OPERATIONS: u8 = 7;

fuzz_target!(|input: &[u8]| {
    let Some((&mode, rest)) = input.split_first() else {
        return;
    };
    let kill_after = usize::from(mode % (MOST_OPERATIONS + 1));
    let mut file = Vec::new();
    let mut input = Unstructured::new(rest);
    // An input that runs out ends its file early; that is not a failure.
    let _ = match mode / 8 {
        0..4 => {
            file.extend(rest);
            Ok(())
        }
        4..16 => wav_file::build_file(&mut input, &mut file),
        _ => written_file(&mut input, &mut file),
    };
    check(&file, kill_after);
});

/// Writes a file through Magnolia's writer, finished or as a writer that died leaves it, sometimes
/// with a `bext` chunk, then hurts it up to three times: cut anywhere; a size set to 0, 0xFFFFFFFF
/// or anything; its id turned to RF64 or BW64; its `JUNK` chunk turned to `ds64`, its sizes 0 or
/// anything; bytes appended.
fn written_file(input: &mut Unstructured<'_>, file: &mut Vec<u8>) -> Result<()> {
    let format = *input.choose(&[
        SampleFormat::Int16,
        SampleFormat::Int24,
        SampleFormat::Int32,
        SampleFormat::Float32,
    ])?;
    let channels: u16 = input.int_in_range(1..=4)?;
    let frames: usize = input.int_in_range(0..=64)?;
    let seed: u32 = input.arbitrary()?;
    // Samples that differ from one another, so that a frame moved shows.
    let samples: Vec<f32> = (0..frames * usize::from(channels))
        .map(|index| {
            let step = (index as u32)
                .wrapping_mul(2_654_435_761)
                .wrapping_add(seed);
            f32::from(step as u16) / 65_536.0 - 0.5
        })
        .collect();
    let bext = input.arbitrary::<bool>()?.then_some(Bext {
        originator: "Magnolia",
        originator_reference: "take",
        time_reference: input.arbitrary()?,
        ..Bext::default()
    });
    let finished = input.ratio(1, 4)?;

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = match &bext {
            Some(bext) => WavWriter::with_bext(&mut cursor, format, channels, 48_000, bext),
            None => WavWriter::new(&mut cursor, format, channels, 48_000),
        }
        .expect("a header the writer takes");
        writer.write_frames(&samples).expect("written to memory");
        if finished {
            writer.finish().expect("finished in memory");
        }
    }
    *file = cursor.into_inner();
    let data_size_at = wav::read_info(&mut Cursor::new(file.as_slice()))
        .expect("the writer's file reads")
        .data_offset as usize
        - 4;

    for _ in 0..input.int_in_range(0..=3)? {
        match input.int_in_range(0..=4)? {
            0 => file.truncate(input.int_in_range(0..=file.len())?),
            1 => {
                let at = *input.choose(&[4, data_size_at])?;
                let anything = input.arbitrary()?;
                let size: u32 = *input.choose(&[0, u32::MAX, anything])?;
                set(file, at, &size.to_le_bytes());
            }
            2 => set(file, 0, input.choose(&[*b"RF64", *b"BW64"])?),
            3 => {
                set(file, 12, b"ds64");
                let sizes: [u8; 16] = if input.arbitrary()? {
                    [0; 16]
                } else {
                    input.arbitrary()?
                };
                set(file, 20, &sizes);
            }
            _ => {
                let len = input.int_in_range(0..=16)?.min(input.len());
                file.extend(input.bytes(len)?);
            }
        }
    }
    Ok(())
}

/// Writes `bytes` over the file's from `at`, as far as the file goes.
fn set(file: &mut [u8], at: usize, bytes: &[u8]) {
    for (byte, &new) in file.iter_mut().skip(at).zip(bytes) {
        *byte = new;
    }
}

fn check(file: &[u8], kill_after: usize) {
    let before = wav::read_info(&mut Cursor::new(file));
    let mut repaired = MemoryFile::new(file, usize::MAX);
    match wav::repair(&mut repaired) {
        Err(error) => {
            assert_eq!(
                repaired.contents(),
                file,
                "a file refused is left as it was"
            );
            match before {
                Ok(_) => assert!(matches!(error, WavError::Unrepairable), "{error:?}"),
                Err(before) => assert_eq!(error.to_string(), before.to_string()),
            }
        }
        Ok(Repair::Untouched) => {
            assert_eq!(repaired.contents(), file, "a file left is left as it was");
            let before = before.expect("a file the repair left reads");
            assert!(!before.length_repaired, "{before:?}");
        }
        Ok(Repair::Repaired { frames, bytes_cut }) => {
            let before = before.expect("a file the repair repaired reads");
            check_repaired(file, &before, repaired.contents(), frames, bytes_cut);
            check_killed(file, repaired.contents(), kill_after);
        }
    }
}

/// The repaired file reads as finished, its whole frames those of the file before, exact, its bext
/// marks kept; a second repair changes no byte.
fn check_repaired(file: &[u8], before: &WavInfo, repaired: &[u8], frames: u64, bytes_cut: u64) {
    let info = wav::read_info(&mut Cursor::new(repaired)).expect("a repaired file reads");
    assert!(!info.length_repaired, "{info:?}");
    assert_eq!(info.frames, frames);
    assert_eq!(before.frames, frames, "every whole frame is kept");
    assert_eq!(info.data_offset, before.data_offset);
    assert_eq!(info.bext, before.bext);
    assert_eq!(info.sample_format, before.sample_format);
    assert_eq!(info.channels, before.channels);
    assert_eq!(info.sample_rate, before.sample_rate);

    let start = before.data_offset as usize;
    let frame_bytes = usize::from(info.channels) * info.sample_format.bytes_per_sample();
    let end = start + frames as usize * frame_bytes;
    assert_eq!(repaired[start..end], file[start..end], "the frames, exact");
    let pad = (end - start) % 2 == 1;
    assert_eq!(repaired.len(), end + usize::from(pad));
    if pad {
        assert_eq!(repaired[end], 0, "the pad byte");
    }
    assert_eq!(bytes_cut, file.len().saturating_sub(repaired.len()) as u64);

    let mut again = MemoryFile::new(repaired, 0);
    assert_eq!(
        wav::repair(&mut again).expect("a repaired file reads"),
        Repair::Untouched
    );
}

/// A repair killed after `kill_after` operations leaves a file that a second repair takes to
/// `repaired`.
fn check_killed(file: &[u8], repaired: &[u8], kill_after: usize) {
    let mut killed = MemoryFile::new(file, kill_after);
    if let Err(error) = wav::repair(&mut killed) {
        assert!(matches!(error, WavError::Io(_)), "{error:?}");
        let mut taken_up = MemoryFile::new(killed.contents(), usize::MAX);
        wav::repair(&mut taken_up).expect("a repair taken up");
        assert_eq!(taken_up.contents(), repaired, "killed after {kill_after}");
    } else {
        assert_eq!(killed.contents(), repaired);
    }
}

/// A file in memory that takes so many writes, cuts and syncs, then fails every one, as the file
/// of a process killed then.
struct MemoryFile {
    file: Cursor<Vec<u8>>,
    operations_left: usize,
}

impl MemoryFile {
    fn new(bytes: &[u8], operations_left: usize) -> Self {
        Self {
            file: Cursor::new(bytes.to_vec()),
            operations_left,
        }
    }

    fn contents(&self) -> &[u8] {
        self.file.get_ref()
    }

    fn operation(&mut self) -> io::Result<()> {
        self.operations_left = self
            .operations_left
            .checked_sub(1)
            .ok_or_else(|| io::Error::other("killed"))?;
        Ok(())
    }
}

impl Read for MemoryFile {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.file.read(bytes)
    }
}

impl Write for MemoryFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.operation()?;
        self.file.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for MemoryFile {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.file.seek(to)
    }
}

impl RepairableFile for MemoryFile {
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.operation()?;
        let len = usize::try_from(len).map_err(io::Error::other)?;
        self.file.get_mut().resize(len, 0);
        Ok(())
    }

    fn sync(&mut self) -> io::Result<()> {
        self.operation()
    }
}
