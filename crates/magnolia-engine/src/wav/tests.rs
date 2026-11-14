//! The paths from RIFF to RF64 of the writer and of the repair, reached by lowering their RIFF size
//! limit rather than by writing 4 GiB. The rest of the module is tested through its public
//! interface, in `tests/wav.rs`.

use std::error::Error;
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};

use super::repair::repair_with_riff_size_limit;
use super::{
    Repair, RepairableFile, SampleFormat, WavError, WavInfo, WavWriter, decode, read_info,
};

type TestResult = Result<(), Box<dyn Error>>;

/// Five frames of three channels in 24 bits: 45 bytes of data, odd, so a pad byte follows them.
/// Its RIFF size is 72 bytes of header after the size field, the data, and the pad byte.
const CHANNELS: u16 = 3;
const FRAMES: u64 = 5;
const DATA_BYTES: u64 = 45;
const RIFF_SIZE: u64 = 118;

/// The samples written: steps of 24 bits, which the file holds exactly.
fn samples() -> Vec<f32> {
    (-7_i16..8).map(|step| f32::from(step) / 16.0).collect()
}

/// Writes the samples with the writer's RIFF size limit lowered to `limit`, and returns the file.
fn write_with_limit(limit: u64) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut writer = WavWriter::new(
        Cursor::new(Vec::new()),
        SampleFormat::Int24,
        CHANNELS,
        96_000,
    )?
    .with_riff_size_limit(limit);
    writer.write_frames(&samples())?;
    Ok(writer.finish()?.into_inner())
}

fn u32_at(file: &[u8], at: usize) -> Option<u32> {
    file.get(at..)?
        .first_chunk()
        .copied()
        .map(u32::from_le_bytes)
}

fn u64_at(file: &[u8], at: usize) -> Option<u64> {
    file.get(at..)?
        .first_chunk()
        .copied()
        .map(u64::from_le_bytes)
}

/// The file reads back as written: its format, its frames, and its samples bit for bit.
fn check_reads_back(file: &[u8]) -> TestResult {
    let info = read_info(&mut Cursor::new(file))?;
    assert_eq!(
        info,
        WavInfo {
            sample_format: SampleFormat::Int24,
            channels: CHANNELS,
            sample_rate: 96_000,
            frames: FRAMES,
            data_offset: 80,
            length_repaired: false,
            bext: None,
        }
    );
    let data = file.get(80..125).ok_or("the data is missing")?;
    let mut decoded = vec![0.0; 15];
    assert_eq!(decode(info.sample_format, CHANNELS, data, &mut decoded), 5);
    assert_eq!(decoded, samples());
    Ok(())
}

#[test]
fn a_file_past_the_limit_is_rewritten_as_rf64_with_its_sizes_in_ds64() -> TestResult {
    let file = write_with_limit(RIFF_SIZE - 1)?;

    assert_eq!(file.len(), 126);
    assert_eq!(file.get(0..4), Some(b"RF64".as_slice()));
    assert_eq!(u32_at(&file, 4), Some(u32::MAX));
    assert_eq!(file.get(8..16), Some(b"WAVEds64".as_slice()));
    assert_eq!(u32_at(&file, 16), Some(28));
    assert_eq!(u64_at(&file, 20), Some(RIFF_SIZE));
    assert_eq!(u64_at(&file, 28), Some(DATA_BYTES));
    assert_eq!(u64_at(&file, 36), Some(FRAMES));
    assert_eq!(u32_at(&file, 44), Some(0), "the table is empty");
    assert_eq!(file.get(48..52), Some(b"fmt ".as_slice()));
    assert_eq!(file.get(72..76), Some(b"data".as_slice()));
    assert_eq!(u32_at(&file, 76), Some(u32::MAX));
    assert_eq!(file.last(), Some(&0), "the pad byte");
    check_reads_back(&file)
}

#[test]
fn a_file_at_the_limit_stays_riff() -> TestResult {
    let file = write_with_limit(RIFF_SIZE)?;

    assert_eq!(file.len(), 126);
    assert_eq!(file.get(0..4), Some(b"RIFF".as_slice()));
    assert_eq!(u32_at(&file, 4), Some(118));
    assert_eq!(file.get(8..16), Some(b"WAVEJUNK".as_slice()));
    assert_eq!(u32_at(&file, 16), Some(28));
    assert_eq!(u32_at(&file, 76), Some(45));
    check_reads_back(&file)
}

// The repair.

/// The samples' file as a writer that died after them leaves it, its sizes unknown.
fn write_unfinished() -> Result<Vec<u8>, Box<dyn Error>> {
    let mut file = Cursor::new(Vec::new());
    {
        let mut writer = WavWriter::new(&mut file, SampleFormat::Int24, CHANNELS, 96_000)?;
        writer.write_frames(&samples())?;
        // Dropped without `finish`, as a crash leaves it.
    }
    Ok(file.into_inner())
}

/// Writes `bytes` over the file's, from `at`.
fn replace(file: &mut [u8], at: usize, bytes: &[u8]) -> TestResult {
    file.get_mut(at..)
        .and_then(|rest| rest.get_mut(..bytes.len()))
        .ok_or("past the end of the file")?
        .copy_from_slice(bytes);
    Ok(())
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

/// Repairs `file` under the RIFF size limit `limit`: it must become `expected`, its `frames` kept,
/// and stay so when repaired again, nothing written. A repair killed after any of its writes, cuts
/// and syncs must leave a file that a second repair takes to `expected` too.
fn check_repairs_to(file: &[u8], limit: u64, expected: &[u8], frames: u64) -> TestResult {
    let mut whole = MemoryFile::new(file, usize::MAX);
    let outcome = repair_with_riff_size_limit(&mut whole, limit)?;
    assert!(
        matches!(outcome, Repair::Repaired { frames: kept, .. } if kept == frames),
        "{outcome:?}"
    );
    assert_eq!(whole.file.get_ref().as_slice(), expected);
    let operations = usize::MAX.saturating_sub(whole.operations_left);

    let mut repaired = MemoryFile::new(expected, 0);
    assert_eq!(
        repair_with_riff_size_limit(&mut repaired, limit)?,
        Repair::Untouched
    );

    for allowed in 0..operations {
        let mut killed = MemoryFile::new(file, allowed);
        let error = repair_with_riff_size_limit(&mut killed, limit).err();
        assert!(
            matches!(error, Some(WavError::Io(_))),
            "killed after {allowed} operations: {error:?}"
        );
        let mut taken_up = MemoryFile::new(killed.file.get_ref(), usize::MAX);
        repair_with_riff_size_limit(&mut taken_up, limit)?;
        assert_eq!(
            taken_up.file.get_ref().as_slice(),
            expected,
            "killed after {allowed} operations"
        );
    }
    Ok(())
}

#[test]
fn a_repair_past_the_limit_writes_rf64_as_finish_does() -> TestResult {
    let rf64 = write_with_limit(RIFF_SIZE - 1)?;
    let unfinished = write_unfinished()?;
    check_repairs_to(&unfinished, RIFF_SIZE - 1, &rf64, FRAMES)?;

    // With 4 bytes of a sixth frame of 9, which the pad byte replaces.
    let cut_short = [unfinished.as_slice(), &[1, 2, 3, 4]].concat();
    check_repairs_to(&cut_short, RIFF_SIZE - 1, &rf64, FRAMES)?;

    // As the writer left a file before its sizes said unknown: 0 and 0. The data size becomes
    // 0xFFFFFFFF before the RF64 header is written, or the header would say the data empty.
    let mut sizes_0 = cut_short;
    replace(&mut sizes_0, 4, &[0; 4])?;
    replace(&mut sizes_0, 76, &[0; 4])?;
    check_repairs_to(&sizes_0, RIFF_SIZE - 1, &rf64, FRAMES)
}

#[test]
fn a_repair_at_the_limit_writes_riff() -> TestResult {
    let riff = write_with_limit(RIFF_SIZE)?;
    check_repairs_to(&write_unfinished()?, RIFF_SIZE, &riff, FRAMES)
}

#[test]
fn a_repair_past_the_limit_without_room_for_ds64_leaves_the_file() -> TestResult {
    // The JUNK chunk that reserves the room renamed; then the room kept, as a ds64 chunk, but a
    // bext chunk renamed ds64 after it, which RF64 would read in its place.
    let mut no_room = write_unfinished()?;
    replace(&mut no_room, 12, b"LIST")?;
    let mut second_ds64 = Vec::new();
    {
        let mut writer = WavWriter::with_bext(
            Cursor::new(&mut second_ds64),
            SampleFormat::Int24,
            CHANNELS,
            96_000,
            &super::Bext::default(),
        )?;
        writer.write_frames(&samples())?;
    }
    replace(&mut second_ds64, 12, b"ds64")?;
    replace(&mut second_ds64, 72, b"ds64")?;
    for file in [no_room.clone(), second_ds64] {
        let mut memory = MemoryFile::new(&file, 0);
        let error = repair_with_riff_size_limit(&mut memory, 0).err();
        assert!(matches!(error, Some(WavError::Unrepairable)), "{error:?}");
        assert_eq!(memory.file.get_ref(), &file, "left as it was");
    }

    // Under the limit, RIFF needs no room for ds64.
    let mut riff = write_with_limit(RIFF_SIZE)?;
    replace(&mut riff, 12, b"LIST")?;
    check_repairs_to(&no_room, RIFF_SIZE, &riff, FRAMES)
}
