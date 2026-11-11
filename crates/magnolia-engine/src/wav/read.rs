//! Reading a WAV file's header: how its samples are encoded, and where they lie.

use std::io::{Read, Seek, SeekFrom};

use super::{
    FORMAT_EXTENSIBLE, FORMAT_IEEE_FLOAT, FORMAT_PCM, MAX_CHANNELS, MAX_CHUNKS_WALKED,
    SampleFormat, WavError,
};

/// What a WAV file's header says of its samples, as [`read_info`] found it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WavInfo {
    /// How each sample is encoded.
    pub sample_format: SampleFormat,
    /// The channels of each frame, interleaved: 1 to 1,024.
    pub channels: u16,
    /// The frames per second, above 0.
    pub sample_rate: u32,
    /// The whole frames in the data, none of them past the end of the file.
    pub frames: u64,
    /// Where the data's first byte lies, from the start of the file.
    pub data_offset: u64,
    /// The header's sizes were those a writer that died leaves: a data size of 0xFFFFFFFF, or of 0
    /// with a RIFF size of 0 or 0xFFFFFFFF. The frames were counted to the end of the file.
    pub length_repaired: bool,
}

/// The sub-formats of WAVE_FORMAT_EXTENSIBLE that Magnolia reads, as their GUIDs lie in a file:
/// KSDATAFORMAT_SUBTYPE_PCM and KSDATAFORMAT_SUBTYPE_IEEE_FLOAT.
const SUB_FORMAT_PCM: [u8; 16] = [
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];
const SUB_FORMAT_IEEE_FLOAT: [u8; 16] = [
    0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];

/// The bytes of a `fmt ` chunk's WAVEFORMATEX fields, and of WAVE_FORMAT_EXTENSIBLE's.
const FORMAT_BYTES: u32 = 16;
const EXTENSIBLE_FORMAT_BYTES: u32 = 40;

/// The bytes of a `ds64` chunk that hold the RIFF's size and the data's, 64 bits each.
const DS64_SIZES_BYTES: u32 = 16;

/// The 32-bit size that says the true one lies in the `ds64` chunk; without one, a size a writer
/// that died left.
const SIZE_IN_DS64: u32 = u32::MAX;

/// Reads the header of the WAV file that `reader` holds, from the start of the file, and leaves
/// the reader at the first byte of its data.
///
/// The chunks are walked until both `fmt ` and `data` are found; what follows `data` is read only
/// when `fmt ` comes after it. The end of the file is found by seeking to it; every move after that
/// is a relative seek, so a [`BufReader`](std::io::BufReader) around a file reads a usual header
/// in one read.
///
/// # Errors
///
/// A plain [`WavError`] for a file that is not a WAV file Magnolia reads, is cut short in its
/// header, or contradicts itself; [`WavError::Io`] when reading or seeking fails.
pub fn read_info<R: Read + Seek>(reader: &mut R) -> Result<WavInfo, WavError> {
    let len = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;
    let mut file = FileCursor {
        reader,
        position: 0,
        len,
    };
    let header = read_file_header(&mut file)?;

    let mut format: Option<Format> = None;
    let mut data: Option<DataChunk> = None;
    let mut ds64 = None;
    let mut chunks_walked: u32 = 0;
    loop {
        if let (Some(format), Some(data)) = (format, data) {
            file.seek_to(data.offset)?;
            return Ok(info(format, data, len));
        }
        match file.remaining() {
            0 if format.is_none() => return Err(WavError::MissingFormat),
            0 => return Err(WavError::MissingData),
            1..8 => return Err(WavError::Truncated),
            _ => {}
        }
        if chunks_walked == MAX_CHUNKS_WALKED {
            return Err(WavError::TooManyChunks);
        }
        chunks_walked = chunks_walked.saturating_add(1);
        let id: [u8; 4] = file.read_array()?;
        let size = u32::from_le_bytes(file.read_array()?);
        let body = file.position;
        let body_size = match &id {
            b"data" if data.is_none() => {
                let declared = declared_data_size(size, header.riff_size, ds64);
                data = Some(DataChunk {
                    offset: body,
                    declared,
                });
                if format.is_some() {
                    continue;
                }
                // The format comes after the data, which can be stepped over only if its size is
                // known.
                declared.ok_or(WavError::MissingFormat)?
            }
            b"fmt " if format.is_none() => {
                format = Some(read_format(&mut file, size)?);
                u64::from(size)
            }
            // Read anywhere before the data, as a lenient reader; EBU Tech 3306 puts it first,
            // right after the RF64 header.
            b"ds64" if header.sizes_in_ds64 && data.is_none() => {
                ds64 = Some(read_ds64(&mut file, size)?);
                u64::from(size)
            }
            _ => u64::from(size),
        };
        file.seek_to(next_chunk(body, body_size, len)?)?;
    }
}

/// A reader that knows its position in the file, and where the file ends.
struct FileCursor<'a, R> {
    reader: &'a mut R,
    position: u64,
    len: u64,
}

impl<R: Read + Seek> FileCursor<'_, R> {
    fn remaining(&self) -> u64 {
        self.len.saturating_sub(self.position)
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], WavError> {
        let mut bytes = [0; N];
        self.reader.read_exact(&mut bytes)?;
        self.position = self.position.saturating_add(N as u64);
        Ok(bytes)
    }

    fn seek_to(&mut self, target: u64) -> Result<(), WavError> {
        // A relative seek keeps a BufReader's buffer when the target lies in it; an absolute one
        // would empty it. Positions past i64's range, which no file reaches, seek absolutely.
        let offset = i64::try_from(target)
            .ok()
            .zip(i64::try_from(self.position).ok())
            .and_then(|(target, position)| target.checked_sub(position));
        match offset {
            Some(offset) => self.reader.seek_relative(offset)?,
            None => {
                self.reader.seek(SeekFrom::Start(target))?;
            }
        }
        self.position = target;
        Ok(())
    }
}

/// The `fmt ` chunk's fields that Magnolia uses.
#[derive(Clone, Copy)]
struct Format {
    sample_format: SampleFormat,
    channels: u16,
    sample_rate: u32,
    block_align: u16,
}

/// Where the data lies, and its size as the header declares it: `None` when unknown.
#[derive(Clone, Copy)]
struct DataChunk {
    offset: u64,
    declared: Option<u64>,
}

/// What the file's first 12 bytes say.
struct FileHeader {
    /// Whether the file's sizes may lie in a `ds64` chunk, as in RF64 and BW64.
    sizes_in_ds64: bool,
    /// The RIFF's 32-bit size. It bounds nothing, as a writer that died leaves it wrong; it only
    /// tells such a writer's empty data from a finished file's.
    riff_size: u32,
}

/// The 64-bit sizes of a `ds64` chunk that Magnolia uses.
#[derive(Clone, Copy)]
struct Ds64Sizes {
    riff_size: u64,
    data_size: u64,
}

/// Reads `RIFF`, `RF64` or `BW64`, the RIFF's size and the form `WAVE`.
fn read_file_header<R: Read + Seek>(file: &mut FileCursor<'_, R>) -> Result<FileHeader, WavError> {
    if file.remaining() < 12 {
        return Err(WavError::Truncated);
    }
    let id: [u8; 4] = file.read_array()?;
    let riff_size = u32::from_le_bytes(file.read_array()?);
    let form: [u8; 4] = file.read_array()?;
    let sizes_in_ds64 = match &id {
        b"RIFF" => false,
        b"RF64" | b"BW64" => true,
        _ => return Err(WavError::NotWave),
    };
    if &form != b"WAVE" {
        return Err(WavError::NotWave);
    }
    Ok(FileHeader {
        sizes_in_ds64,
        riff_size,
    })
}

/// Reads a `fmt ` chunk of `size` bytes from its start, and checks it.
fn read_format<R: Read + Seek>(
    file: &mut FileCursor<'_, R>,
    size: u32,
) -> Result<Format, WavError> {
    if size < FORMAT_BYTES {
        return Err(WavError::FormatTooShort(size));
    }
    // WAVEFORMATEX, little-endian: the format tag, the channels, the sample rate, the bytes per
    // second (not used), the block align and the bits per sample.
    let [t0, t1, c0, c1, r0, r1, r2, r3, _, _, _, _, a0, a1, b0, b1] = file.read_array()?;
    let format_tag = u16::from_le_bytes([t0, t1]);
    let channels = u16::from_le_bytes([c0, c1]);
    let sample_rate = u32::from_le_bytes([r0, r1, r2, r3]);
    let block_align = u16::from_le_bytes([a0, a1]);
    let bits_per_sample = u16::from_le_bytes([b0, b1]);

    let unsupported = WavError::UnsupportedFormat {
        format_tag,
        bits_per_sample,
    };
    let float = match format_tag {
        FORMAT_PCM => false,
        FORMAT_IEEE_FLOAT => true,
        FORMAT_EXTENSIBLE => read_sub_format_is_float(file, size)?,
        _ => return Err(unsupported),
    };
    let sample_format = SampleFormat::from_encoding(float, bits_per_sample).ok_or(unsupported)?;
    if !(1..=MAX_CHANNELS).contains(&channels) {
        return Err(WavError::ChannelsOutOfRange(channels));
    }
    let expected = usize::from(channels).saturating_mul(sample_format.bytes_per_sample());
    if usize::from(block_align) != expected {
        return Err(WavError::BlockAlignMismatch {
            block_align,
            expected,
        });
    }
    if sample_rate == 0 {
        return Err(WavError::SampleRateOutOfRange(sample_rate));
    }
    Ok(Format {
        sample_format,
        channels,
        sample_rate,
        block_align,
    })
}

/// Reads the rest of a WAVE_FORMAT_EXTENSIBLE `fmt ` chunk of `size` bytes, after its WAVEFORMATEX
/// fields; returns whether its sub-format is IEEE float rather than integer PCM.
fn read_sub_format_is_float<R: Read + Seek>(
    file: &mut FileCursor<'_, R>,
    size: u32,
) -> Result<bool, WavError> {
    if size < EXTENSIBLE_FORMAT_BYTES {
        return Err(WavError::FormatTooShort(size));
    }
    // The extension's size, the valid bits per sample and the channel mask, none of them used.
    let _extension: [u8; 8] = file.read_array()?;
    match file.read_array()? {
        SUB_FORMAT_PCM => Ok(false),
        SUB_FORMAT_IEEE_FLOAT => Ok(true),
        _ => Err(WavError::UnsupportedSubFormat),
    }
}

/// Reads the RIFF's and the data's 64-bit sizes from a `ds64` chunk of `size` bytes; the sample
/// count and the table that follow them are not used.
fn read_ds64<R: Read + Seek>(
    file: &mut FileCursor<'_, R>,
    size: u32,
) -> Result<Ds64Sizes, WavError> {
    if size < DS64_SIZES_BYTES {
        return Err(WavError::Ds64TooShort(size));
    }
    Ok(Ds64Sizes {
        riff_size: u64::from_le_bytes(file.read_array()?),
        data_size: u64::from_le_bytes(file.read_array()?),
    })
}

/// The data's size as the header declares it, or `None` when a writer that died left it unknown.
///
/// Such a writer leaves the data size at 0xFFFFFFFF, or leaves both the data size and the RIFF size
/// unwritten: 0, or 0xFFFFFFFF. A data size of 0 beside a RIFF size that was written is a finished
/// file without frames, whatever chunks follow its data. In RF64 and BW64, a 32-bit size of
/// 0xFFFFFFFF stands for the `ds64` chunk's 64-bit one.
fn declared_data_size(data_size: u32, riff_size: u32, ds64: Option<Ds64Sizes>) -> Option<u64> {
    let data_size = match (data_size, ds64) {
        (SIZE_IN_DS64, Some(ds64)) => ds64.data_size,
        (SIZE_IN_DS64, None) => return None,
        (size, _) => u64::from(size),
    };
    // An unwritten RIFF size counts as 0.
    let riff_size = match (riff_size, ds64) {
        (SIZE_IN_DS64, Some(ds64)) => ds64.riff_size,
        (SIZE_IN_DS64, None) => 0,
        (size, _) => u64::from(size),
    };
    if data_size == 0 && riff_size == 0 {
        None
    } else {
        Some(data_size)
    }
}

/// Where the chunk after the one whose body starts at `body` lies, past its pad byte when its size
/// is odd. A chunk cut short by the end of the file leaves no room for the chunks still sought.
fn next_chunk(body: u64, size: u64, len: u64) -> Result<u64, WavError> {
    let end = body
        .checked_add(size)
        .filter(|&end| end <= len)
        .ok_or(WavError::Truncated)?;
    Ok(end.saturating_add(size & 1))
}

/// What the header says, its data cut to the end of the file and to whole frames.
fn info(format: Format, data: DataChunk, len: u64) -> WavInfo {
    let available = len.saturating_sub(data.offset);
    let bytes = data
        .declared
        .map_or(available, |declared| declared.min(available));
    WavInfo {
        sample_format: format.sample_format,
        channels: format.channels,
        sample_rate: format.sample_rate,
        frames: bytes
            .checked_div(u64::from(format.block_align))
            .unwrap_or(0),
        data_offset: data.offset,
        length_repaired: data.declared.is_none(),
    }
}
