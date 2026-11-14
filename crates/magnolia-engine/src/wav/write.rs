//! Writing a WAV file: RIFF, turned into RF64 when it passes 4 GiB.

use std::io::{Seek, SeekFrom, Write};

use super::bext::{BEXT_BYTES, Bext};
use super::{
    FORMAT_IEEE_FLOAT, FORMAT_PCM, MAX_CHANNELS, Magic, SampleFormat, UNKNOWN_SIZE, WavError,
};

/// The bytes of a `ds64` chunk's body: the RIFF's size, the data's, the frames, and the length of
/// a table left empty (EBU Tech 3306). The writer reserves them as a `JUNK` chunk right after the
/// file's first 12 bytes.
pub(super) const DS64_BODY_BYTES: u32 = 28;

/// The samples encoded at once: the bytes of a piece, at most 16 KiB, go out in one write.
const SAMPLES_PER_PIECE: usize = 4_096;

/// Writes a WAV file of one [`SampleFormat`], from interleaved `f32` frames.
///
/// [`new`](Self::new) writes the header at the writer's position: `RIFF`, a `JUNK` chunk of 28
/// bytes, `fmt ` (format tag 1 for integers in 16 bytes, 3 for floats in 18), then `data`.
/// [`with_bext`](Self::with_bext) writes a `bext` chunk of 602 bytes between `fmt ` and `data`.
/// [`write_frames`](Self::write_frames) appends samples; [`finish`](Self::finish) writes the
/// sizes. A file past 4 GiB becomes RF64 then: its `JUNK` chunk is overwritten by a `ds64` chunk
/// of the same size, which holds the 64-bit sizes (EBU Tech 3306).
///
/// Integer samples are scaled by their full scale (32,768, 8,388,608 or 2,147,483,648), rounded
/// to the nearest integer, ties away from zero, and clamped to the format's range; a NaN becomes 0.
/// Floats are written as given. So the 16- and 24-bit samples and the floats that
/// [`decode`](super::decode) gave are written back bit for bit; 32-bit integers come back as
/// `decode` rounded them, to `f32`'s 24 bits of precision.
///
/// Until `finish`, the header's RIFF and data sizes are 0xFFFFFFFF, which says that they are not
/// known: if the writer dies first, its file reads to its end, in Magnolia, where
/// [`read_info`](super::read_info) says its length was repaired, as in FFmpeg, SoX and libsndfile
/// ([ADR-0030](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0030-takes-in-wav-of-unknown-length-repaired-on-recovery.md));
/// [`repair`](super::repair) then writes its sizes.
///
/// Once a write has failed, the writer refuses every later `write_frames` and `finish` with
/// [`WavError::WriterFailed`]: what was written may end inside a sample, and more frames after it
/// would be read shifted. The file is left as a writer that died leaves it, its sizes unknown, so
/// its whole frames read to its end.
///
/// Each call writes its samples in pieces of up to 16 KiB: wrap a file in a
/// [`BufWriter`](std::io::BufWriter) to write fewer, larger blocks.
pub struct WavWriter<W: Write + Seek> {
    writer: W,
    format: SampleFormat,
    channels: u16,
    /// Where the header starts: the writer's position when this was made.
    start: u64,
    /// The header's bytes, up to the first byte of the data.
    header_bytes: u64,
    /// The bytes of the samples written so far.
    data_bytes: u64,
    /// The largest RIFF size a RIFF header is given; past it, `finish` writes RF64. Only tests
    /// lower it from the 32-bit field's largest value.
    riff_size_limit: u64,
    /// One piece of samples, encoded; kept to be reused.
    piece: Vec<u8>,
    /// Whether a write failed, after which nothing more is written.
    failed: bool,
}

impl<W: Write + Seek> WavWriter<W> {
    /// Writes the header of a file of `channels` channels, 1 to 1,024, at `sample_rate` hertz.
    ///
    /// # Errors
    ///
    /// [`WavError::ChannelsOutOfRange`]; [`WavError::SampleRateOutOfRange`] for 0 Hz, or a rate
    /// whose bytes per second overflow their 32-bit field; [`WavError::Io`] when writing fails.
    pub fn new(
        writer: W,
        format: SampleFormat,
        channels: u16,
        sample_rate: u32,
    ) -> Result<Self, WavError> {
        let header = header(format, channels, sample_rate, None)?;
        Self::with_header(writer, format, channels, &header)
    }

    /// Writes the header of a file as [`new`](Self::new) does, with `bext` as its `bext` chunk
    /// (EBU Tech 3285, version 2), as [`Bext`] says, between its `fmt ` and `data` chunks.
    ///
    /// # Errors
    ///
    /// Those of `new`; [`WavError::BextFieldNotAscii`] or [`WavError::BextFieldTooLong`] for a
    /// text of `bext` that its field cannot hold. Nothing is written when the header is refused.
    pub fn with_bext(
        writer: W,
        format: SampleFormat,
        channels: u16,
        sample_rate: u32,
        bext: &Bext<'_>,
    ) -> Result<Self, WavError> {
        let header = header(format, channels, sample_rate, Some(bext))?;
        Self::with_header(writer, format, channels, &header)
    }

    fn with_header(
        mut writer: W,
        format: SampleFormat,
        channels: u16,
        header: &[u8],
    ) -> Result<Self, WavError> {
        let start = writer.stream_position()?;
        writer.write_all(header)?;
        Ok(Self {
            writer,
            format,
            channels,
            start,
            header_bytes: header.len() as u64,
            data_bytes: 0,
            riff_size_limit: u64::from(u32::MAX),
            piece: Vec::with_capacity(SAMPLES_PER_PIECE.saturating_mul(4)),
            failed: false,
        })
    }

    /// Appends whole frames, their channels interleaved.
    ///
    /// # Errors
    ///
    /// [`WavError::WriterFailed`], writing nothing, once a write has failed;
    /// [`WavError::PartialFrame`], writing nothing, when the samples are not whole frames;
    /// [`WavError::Io`] when writing fails, which may leave part of the samples written;
    /// [`WavError::FileTooLarge`].
    pub fn write_frames(&mut self, interleaved: &[f32]) -> Result<(), WavError> {
        if self.failed {
            return Err(WavError::WriterFailed);
        }
        if interleaved.len().checked_rem(usize::from(self.channels)) != Some(0) {
            return Err(WavError::PartialFrame {
                samples: interleaved.len(),
                channels: self.channels,
            });
        }
        for samples in interleaved.chunks(SAMPLES_PER_PIECE) {
            self.piece.clear();
            let encoded = samples.iter().copied();
            match self.format {
                SampleFormat::Int16 => self.piece.extend(encoded.flat_map(encode_int16)),
                SampleFormat::Int24 => self.piece.extend(encoded.flat_map(encode_int24)),
                SampleFormat::Int32 => self.piece.extend(encoded.flat_map(encode_int32)),
                SampleFormat::Float32 => self.piece.extend(encoded.flat_map(f32::to_le_bytes)),
            }
            let data_bytes = self
                .data_bytes
                .checked_add(self.piece.len() as u64)
                .ok_or(WavError::FileTooLarge)?;
            if let Err(error) = self.writer.write_all(&self.piece) {
                self.failed = true;
                return Err(error.into());
            }
            self.data_bytes = data_bytes;
        }
        Ok(())
    }

    /// Writes the pad byte after odd data, then the sizes, the data's last, so that a file cut
    /// short between these writes still reads as unfinished: as RIFF, if they fit its 32-bit
    /// fields, the RIFF size then the data size; as RF64 otherwise, the header's first 48 bytes in
    /// one write, the data size left at 0xFFFFFFFF. Leaves the writer at the end of the file,
    /// flushed, and returns it. Nothing is synced: sync the data before `finish`, so that no size
    /// claims bytes that did not reach the disk, and the file after it, for its sizes to outlast a
    /// power cut.
    ///
    /// # Errors
    ///
    /// [`WavError::WriterFailed`], writing nothing, once a write has failed; [`WavError::Io`] when
    /// writing, seeking or flushing fails; [`WavError::FileTooLarge`].
    pub fn finish(mut self) -> Result<W, WavError> {
        if self.failed {
            return Err(WavError::WriterFailed);
        }
        let pad = self.data_bytes & 1;
        if pad == 1 {
            self.writer.write_all(&[0])?;
        }
        // The bytes after the RIFF's size field: the form, the chunks and the pad byte.
        let riff_size = self
            .header_bytes
            .checked_sub(8)
            .and_then(|size| size.checked_add(self.data_bytes))
            .and_then(|size| size.checked_add(pad))
            .ok_or(WavError::FileTooLarge)?;
        let sizes = Sizes {
            riff: riff_size,
            data: self.data_bytes,
            frames: self
                .data_bytes
                .checked_div(self.bytes_per_frame())
                .unwrap_or(0),
        };
        let end = self.at(riff_size.checked_add(8).ok_or(WavError::FileTooLarge)?)?;
        let data_size_at = self.at(self.header_bytes.saturating_sub(4))?;
        let fields = SizeFields::choose(Magic::Riff, sizes, self.riff_size_limit);
        write_sizes(&mut self.writer, self.start, data_size_at, sizes, fields)?;
        self.writer.seek(SeekFrom::Start(end))?;
        self.writer.flush()?;
        Ok(self.writer)
    }

    /// The position in the writer of `offset` from the header's start.
    fn at(&self, offset: u64) -> Result<u64, WavError> {
        self.start.checked_add(offset).ok_or(WavError::FileTooLarge)
    }

    fn bytes_per_frame(&self) -> u64 {
        u64::from(self.channels).saturating_mul(self.format.bytes_per_sample() as u64)
    }
}

#[cfg(test)]
impl<W: Write + Seek> WavWriter<W> {
    /// Lowers the RIFF size past which [`finish`](Self::finish) writes RF64, so that a test reaches
    /// RF64 without writing 4 GiB.
    pub(super) fn with_riff_size_limit(mut self, limit: u64) -> Self {
        self.riff_size_limit = limit;
        self
    }
}

/// The header of a file, its sizes 0xFFFFFFFF until `finish` writes them, with a `bext` chunk
/// when one is given.
fn header(
    format: SampleFormat,
    channels: u16,
    sample_rate: u32,
    bext: Option<&Bext<'_>>,
) -> Result<Vec<u8>, WavError> {
    if !(1..=MAX_CHANNELS).contains(&channels) {
        return Err(WavError::ChannelsOutOfRange(channels));
    }
    // At most 1,024 channels of 4 bytes: the block align fits its 16 bits.
    let block_align = channels.saturating_mul(format.bits_per_sample() / 8);
    let bytes_per_second = sample_rate
        .checked_mul(u32::from(block_align))
        .filter(|_| sample_rate > 0)
        .ok_or(WavError::SampleRateOutOfRange(sample_rate))?;
    // A float's WAVEFORMATEX carries the size of its extension, none; integer PCM's omits it.
    let (format_tag, extension): (u16, &[u8]) = if format.is_float() {
        (FORMAT_IEEE_FLOAT, &[0, 0])
    } else {
        (FORMAT_PCM, &[])
    };
    let fmt_size: u32 = if format.is_float() { 18 } else { 16 };
    let bext_chunk = match bext {
        Some(bext) => [b"bext".as_slice(), &BEXT_BYTES.to_le_bytes(), &bext.body()?].concat(),
        None => Vec::new(),
    };
    Ok([
        b"RIFF".as_slice(),
        &UNKNOWN_SIZE.to_le_bytes(),
        b"WAVE",
        b"JUNK",
        &DS64_BODY_BYTES.to_le_bytes(),
        &[0; DS64_BODY_BYTES as usize],
        b"fmt ",
        &fmt_size.to_le_bytes(),
        &format_tag.to_le_bytes(),
        &channels.to_le_bytes(),
        &sample_rate.to_le_bytes(),
        &bytes_per_second.to_le_bytes(),
        &block_align.to_le_bytes(),
        &format.bits_per_sample().to_le_bytes(),
        extension,
        &bext_chunk,
        b"data",
        &UNKNOWN_SIZE.to_le_bytes(),
    ]
    .concat())
}

/// The sizes of a finished file, as its header gives them.
#[derive(Clone, Copy)]
pub(super) struct Sizes {
    /// The bytes after the RIFF's size field: the form, the chunks and the data's pad byte.
    pub(super) riff: u64,
    /// The bytes of the data, without its pad byte.
    pub(super) data: u64,
    /// The whole frames of the data.
    pub(super) frames: u64,
}

/// Where a finished header holds its sizes.
#[derive(Clone, Copy)]
pub(super) enum SizeFields {
    /// In RIFF's 32-bit fields.
    Riff { riff: u32, data: u32 },
    /// In a `ds64` chunk of 28 bytes right after the file's first 12 bytes, which start with
    /// `magic`; the 32-bit sizes say 0xFFFFFFFF.
    Ds64 { magic: Magic },
}

impl SizeFields {
    /// RIFF's 32-bit fields for a RIFF file whose sizes fit them, its RIFF size at most
    /// `riff_size_limit`; `ds64` otherwise, a RIFF file becoming RF64, RF64 and BW64 keeping their
    /// ids.
    pub(super) fn choose(magic: Magic, sizes: Sizes, riff_size_limit: u64) -> Self {
        if magic == Magic::Riff
            && sizes.riff <= riff_size_limit
            && let (Ok(riff), Ok(data)) = (u32::try_from(sizes.riff), u32::try_from(sizes.data))
        {
            return Self::Riff { riff, data };
        }
        let magic = if magic == Magic::Riff {
            Magic::Rf64
        } else {
            magic
        };
        Self::Ds64 { magic }
    }
}

/// Writes `sizes` into the header that starts at `start`, whose data size field lies at
/// `data_size_at`, as `fields` places them, the data's last: in RIFF, the RIFF size then the data
/// size; in `ds64`, the header's first 48 bytes in one write, the data size left as it is. The
/// header must say unfinished before: in RIFF, a data size of 0xFFFFFFFF; in RF64 and BW64, `ds64`
/// sizes of 0, or a `JUNK` chunk in place of `ds64`. So a file cut short between the writes still
/// reads as unfinished.
pub(super) fn write_sizes<W: Write + Seek>(
    writer: &mut W,
    start: u64,
    data_size_at: u64,
    sizes: Sizes,
    fields: SizeFields,
) -> Result<(), WavError> {
    match fields {
        SizeFields::Riff { riff, data } => {
            let riff_size_at = start.checked_add(4).ok_or(WavError::FileTooLarge)?;
            write_at(writer, riff_size_at, &riff.to_le_bytes())?;
            write_at(writer, data_size_at, &data.to_le_bytes())?;
        }
        SizeFields::Ds64 { magic } => {
            // The 32-bit RIFF size says that the true one lies in the ds64 chunk, which replaces
            // the JUNK chunk reserved for it.
            let header = [
                magic.id().as_slice(),
                &UNKNOWN_SIZE.to_le_bytes(),
                b"WAVE",
                b"ds64",
                &DS64_BODY_BYTES.to_le_bytes(),
                &sizes.riff.to_le_bytes(),
                &sizes.data.to_le_bytes(),
                &sizes.frames.to_le_bytes(),
                &0_u32.to_le_bytes(),
            ]
            .concat();
            write_at(writer, start, &header)?;
        }
    }
    Ok(())
}

/// Writes `bytes` at `position`.
pub(super) fn write_at<W: Write + Seek>(
    writer: &mut W,
    position: u64,
    bytes: &[u8],
) -> Result<(), WavError> {
    writer.seek(SeekFrom::Start(position))?;
    writer.write_all(bytes)?;
    Ok(())
}

// Each integer encoder scales, rounds, then converts with `as`, which saturates to the integer's
// range and turns a NaN into 0. 24 bits need their own clamp, being narrower than an i32.

fn encode_int16(sample: f32) -> [u8; 2] {
    ((sample * 32_768.0).round() as i16).to_le_bytes()
}

fn encode_int24(sample: f32) -> [u8; 3] {
    let value = (sample * 8_388_608.0)
        .round()
        .clamp(-8_388_608.0, 8_388_607.0) as i32;
    let [low, middle, high, _] = value.to_le_bytes();
    [low, middle, high]
}

fn encode_int32(sample: f32) -> [u8; 4] {
    ((sample * 2_147_483_648.0).round() as i32).to_le_bytes()
}
