//! The writer's RF64 path, reached by lowering its RIFF size limit rather than by writing 4 GiB.
//! The rest of the module is tested through its public interface, in `tests/wav.rs`.

use std::error::Error;
use std::io::Cursor;

use super::{SampleFormat, WavInfo, WavWriter, decode, read_info};

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
