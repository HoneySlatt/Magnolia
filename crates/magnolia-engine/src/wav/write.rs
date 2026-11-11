//! Writing a WAV file: RIFF, turned into RF64 when it passes 4 GiB.

use std::io::{Seek, SeekFrom, Write};

use super::{FORMAT_IEEE_FLOAT, FORMAT_PCM, MAX_CHANNELS, SampleFormat, WavError};

/// The bytes of a `ds64` chunk's body: the RIFF's size, the data's, the frames, and the length of
/// a table left empty (EBU Tech 3306). The writer reserves them as a `JUNK` chunk.
const DS64_BODY_BYTES: u32 = 28;

/// The samples encoded at once: the bytes of a piece, at most 16 KiB, go out in one write.
const SAMPLES_PER_PIECE: usize = 4_096;

/// Writes a WAV file of one [`SampleFormat`], from interleaved `f32` frames.
///
/// [`new`](Self::new) writes the header at the writer's position: `RIFF`, a `JUNK` chunk of 28
/// bytes, `fmt ` (format tag 1 for integers in 16 bytes, 3 for floats in 18), then `data`.
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
/// Until `finish`, the header's RIFF and data sizes are 0: if the writer dies first, the file reads
/// to its end, and [`read_info`](super::read_info) says its length was repaired.
///
/// Once a write has failed, the writer refuses every later `write_frames` and `finish` with
/// [`WavError::WriterFailed`]: what was written may end inside a sample, and more frames after it
/// would be read shifted. The file is left as a writer that died leaves it, its sizes 0, so its
/// whole frames read to its end.
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
        mut writer: W,
        format: SampleFormat,
        channels: u16,
        sample_rate: u32,
    ) -> Result<Self, WavError> {
        let header = header(format, channels, sample_rate)?;
        let start = writer.stream_position()?;
        writer.write_all(&header)?;
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

    /// Writes the pad byte after odd data, then the sizes: as RIFF if they fit its 32-bit fields,
    /// as RF64 otherwise. Leaves the writer at the end of the file, flushed, and returns it.
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
        let end = self.at(riff_size.checked_add(8).ok_or(WavError::FileTooLarge)?)?;
        let data_size_at = self.at(self.header_bytes.saturating_sub(4))?;

        match (u32::try_from(riff_size), u32::try_from(self.data_bytes)) {
            (Ok(riff_size_32), Ok(data_size_32)) if riff_size <= self.riff_size_limit => {
                let riff_size_at = self.at(4)?;
                self.write_at(riff_size_at, &riff_size_32.to_le_bytes())?;
                self.write_at(data_size_at, &data_size_32.to_le_bytes())?;
            }
            _ => {
                let frames = self
                    .data_bytes
                    .checked_div(self.bytes_per_frame())
                    .unwrap_or(0);
                // RF64's 32-bit sizes say that the true ones lie in the ds64 chunk.
                let rf64_header = [
                    b"RF64".as_slice(),
                    &u32::MAX.to_le_bytes(),
                    b"WAVE",
                    b"ds64",
                    &DS64_BODY_BYTES.to_le_bytes(),
                    &riff_size.to_le_bytes(),
                    &self.data_bytes.to_le_bytes(),
                    &frames.to_le_bytes(),
                    &0_u32.to_le_bytes(),
                ]
                .concat();
                self.write_at(self.start, &rf64_header)?;
                self.write_at(data_size_at, &u32::MAX.to_le_bytes())?;
            }
        }
        self.writer.seek(SeekFrom::Start(end))?;
        self.writer.flush()?;
        Ok(self.writer)
    }

    /// The position in the writer of `offset` from the header's start.
    fn at(&self, offset: u64) -> Result<u64, WavError> {
        self.start.checked_add(offset).ok_or(WavError::FileTooLarge)
    }

    fn write_at(&mut self, position: u64, bytes: &[u8]) -> Result<(), WavError> {
        self.writer.seek(SeekFrom::Start(position))?;
        self.writer.write_all(bytes)?;
        Ok(())
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

/// The header of a file, its sizes 0 until `finish` writes them.
fn header(format: SampleFormat, channels: u16, sample_rate: u32) -> Result<Vec<u8>, WavError> {
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
    Ok([
        b"RIFF".as_slice(),
        &0_u32.to_le_bytes(),
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
        b"data",
        &0_u32.to_le_bytes(),
    ]
    .concat())
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
