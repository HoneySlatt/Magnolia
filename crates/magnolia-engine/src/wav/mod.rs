//! WAV files, read, written and repaired by Magnolia's own code
//! ([ADR-0029](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0029-magnolias-own-wav-and-midi-file-readers.md),
//! [ADR-0030](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0030-takes-in-wav-of-unknown-length-repaired-on-recovery.md)).
//!
//! [`read_info`] finds what a file holds and where its samples lie; [`decode`] turns those bytes
//! into `f32` samples; [`WavWriter`] writes a file; [`repair`] finishes a file whose writer died.
//! The disk reader opens each file with `read_info`, then reads its data by blocks and decodes
//! each.
//!
//! What is read:
//!
//! - RIFF/WAVE, and RF64/WAVE or BW64/WAVE, whose `ds64` chunk holds the 64-bit sizes of files past
//!   4 GiB (EBU Tech 3306, ITU-R BS.2088);
//! - a `fmt ` chunk of format tag 1 (integer PCM), 3 (IEEE float) or 0xFFFE
//!   (WAVE_FORMAT_EXTENSIBLE, its sub-format integer PCM or IEEE float), as [`SampleFormat`] lists;
//!   1 to 1,024 channels, a block align of exactly the channels times the bytes of a sample, a
//!   sample rate above 0;
//! - a `bext` chunk before `data` (EBU Tech 3285): its time reference, originator and originator
//!   reference, as [`BextInfo`] gives them; one shorter than the 602 bytes of its fixed part is
//!   skipped as an unknown chunk;
//! - any other chunk skipped, a chunk of odd size followed by one pad byte, chunks before or after
//!   `data`; at most 1,024 chunks before both `fmt ` and `data` are found.
//!
//! A writer that died leaves the data size at 0xFFFFFFFF, or leaves both the data size and the
//! RIFF size unwritten, at 0 or 0xFFFFFFFF. The data is then read to the end of the file, whole
//! frames only, and [`WavInfo::length_repaired`] says so. A data size of 0 beside a RIFF size that
//! was written is a finished file without frames, whatever chunks follow its data. A data size past
//! the end of the file is cut to it. The RIFF size bounds nothing, as a writer that died leaves it
//! wrong.
//!
//! What is written: RIFF, which becomes RF64 when the file passes 4 GiB, in any of the four sample
//! formats, with a `bext` chunk when asked; [`WavWriter`] gives the layout. Its RIFF and data sizes
//! say 0xFFFFFFFF, unknown, until the writer finishes, so that the file of a writer that dies reads
//! whole, here and in the common readers.
//!
//! What is repaired: a file whose sizes a writer that died left, or whose data runs past its end,
//! is cut after its last whole frame and given its sizes, as the writer would have; [`repair`]
//! gives its steps, and why one cut short leaves a file that repairs again.
//!
//! Limits: the `ds64` table, which gives 64-bit sizes to chunks other than `data`, is not read;
//! such a chunk before `fmt ` or `data` makes the file unreadable, or misread. The valid bits of
//! WAVE_FORMAT_EXTENSIBLE are not read: a sample is decoded as its whole container, where its
//! unused low bits are zero. Nor is its channel mask, the speakers of the channels. RIFX, the
//! big-endian variant of WAV, is not read. Of `bext`, only the three marks above are read, and its
//! coding history is never written. The writer never writes WAVE_FORMAT_EXTENSIBLE nor a `fact`
//! chunk, which some strict readers expect above two channels or for floats. [`repair`] writes
//! 64-bit sizes only where Magnolia's writer reserves room for them, right after the file's first
//! 12 bytes; it refuses a file that needs them without that room.

// The disk reader runs this module on every file it opens; a bad file must never crash Magnolia,
// so nothing here may panic. The lints cover its submodules: the reader, the writer, `bext` and
// the repair.
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod bext;
mod read;
mod repair;
mod write;

#[cfg(test)]
mod tests;

use std::error::Error;
use std::fmt;
use std::io;

pub use bext::{Bext, BextInfo};
pub use read::{WavInfo, read_info};
pub use repair::{Repair, RepairableFile, repair};
pub use write::WavWriter;

/// How the samples of a WAV file are encoded, each little-endian.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SampleFormat {
    /// 16-bit signed integers.
    Int16,
    /// 24-bit signed integers, packed in 3 bytes.
    Int24,
    /// 32-bit signed integers.
    Int32,
    /// 32-bit IEEE floats.
    Float32,
}

impl SampleFormat {
    /// The bytes of one sample of one channel.
    pub fn bytes_per_sample(self) -> usize {
        match self {
            Self::Int16 => 2,
            Self::Int24 => 3,
            Self::Int32 | Self::Float32 => 4,
        }
    }

    /// The format of a `fmt ` chunk's encoding, given as integer or float and its bits per sample.
    fn from_encoding(float: bool, bits_per_sample: u16) -> Option<Self> {
        match (float, bits_per_sample) {
            (false, 16) => Some(Self::Int16),
            (false, 24) => Some(Self::Int24),
            (false, 32) => Some(Self::Int32),
            (true, 32) => Some(Self::Float32),
            _ => None,
        }
    }

    fn is_float(self) -> bool {
        self == Self::Float32
    }

    /// The bits of a sample, as a `fmt ` chunk gives them.
    fn bits_per_sample(self) -> u16 {
        match self {
            Self::Int16 => 16,
            Self::Int24 => 24,
            Self::Int32 | Self::Float32 => 32,
        }
    }
}

/// The most channels a WAV file may hold, in reading and in writing.
const MAX_CHANNELS: u16 = 1_024;

/// The most chunks read before both `fmt ` and `data` are found. Real files hold a few dozen at
/// most; the bound keeps a file of zeros, which reads as empty chunks 8 bytes apart, from taking
/// seconds.
const MAX_CHUNKS_WALKED: u32 = 1_024;

/// The 32-bit size 0xFFFFFFFF. In RIFF it says that the size is not known: a writer leaves it so
/// until it finishes, and a writer that died leaves it so for good. In RF64 and BW64 it says that
/// the true size lies in the `ds64` chunk.
const UNKNOWN_SIZE: u32 = u32::MAX;

/// The id a WAV file starts with, which says where its sizes lie.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Magic {
    /// `RIFF`: in 32-bit fields.
    Riff,
    /// `RF64` (EBU Tech 3306): in a `ds64` chunk, where a 32-bit field says 0xFFFFFFFF.
    Rf64,
    /// `BW64` (ITU-R BS.2088): as RF64.
    Bw64,
}

impl Magic {
    fn from_id(id: [u8; 4]) -> Option<Self> {
        match &id {
            b"RIFF" => Some(Self::Riff),
            b"RF64" => Some(Self::Rf64),
            b"BW64" => Some(Self::Bw64),
            _ => None,
        }
    }

    fn id(self) -> [u8; 4] {
        match self {
            Self::Riff => *b"RIFF",
            Self::Rf64 => *b"RF64",
            Self::Bw64 => *b"BW64",
        }
    }

    /// Whether the file's sizes may lie in a `ds64` chunk.
    fn sizes_in_ds64(self) -> bool {
        self != Self::Riff
    }
}

/// The `fmt ` chunk's format tags.
const FORMAT_PCM: u16 = 1;
const FORMAT_IEEE_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;

// The factors that turn an integer sample into a float whose full scale is 1. Each is a power of
// two, so multiplying by it is exact.
const INT16_SCALE: f32 = 1.0 / 32_768.0;
const INT32_SCALE: f32 = 1.0 / 2_147_483_648.0;

/// Decodes whole interleaved frames from `bytes` into `out`, interleaved, and returns the frames
/// decoded: as many as both hold, so 0 when `channels` is 0. Never panics.
///
/// Integers are divided by their full scale: 16-bit by 32,768; 24-bit, sign-extended, by
/// 8,388,608, which is exact in `f32`; 32-bit by 2,147,483,648, rounded to `f32`'s 24 bits of
/// precision. Floats are kept as stored, infinities and NaNs included. The samples of `out` past
/// the frames decoded are left as they were.
pub fn decode(format: SampleFormat, channels: u16, bytes: &[u8], out: &mut [f32]) -> usize {
    let channels = usize::from(channels);
    let Some(frame_bytes) = channels.checked_mul(format.bytes_per_sample()) else {
        return 0;
    };
    let (Some(frames_in_bytes), Some(frames_in_out)) = (
        bytes.len().checked_div(frame_bytes),
        out.len().checked_div(channels),
    ) else {
        return 0;
    };
    let frames = frames_in_bytes.min(frames_in_out);
    let Some(out) = frames
        .checked_mul(channels)
        .and_then(|samples| out.get_mut(..samples))
    else {
        return 0;
    };
    match format {
        SampleFormat::Int16 => decode_samples(bytes, out, |sample| {
            f32::from(i16::from_le_bytes(sample)) * INT16_SCALE
        }),
        // The three bytes placed high in an i32 sign-extend the sample and scale it by 256, which
        // the 32-bit scale undoes. The result has 24 significant bits, so its f32 is exact.
        SampleFormat::Int24 => decode_samples(bytes, out, |[low, middle, high]| {
            i32::from_le_bytes([0, low, middle, high]) as f32 * INT32_SCALE
        }),
        SampleFormat::Int32 => decode_samples(bytes, out, |sample| {
            i32::from_le_bytes(sample) as f32 * INT32_SCALE
        }),
        SampleFormat::Float32 => decode_samples(bytes, out, f32::from_le_bytes),
    }
    frames
}

/// Decodes the samples of `bytes`, each `N` bytes long, into `out`, until either runs out.
fn decode_samples<const N: usize>(
    bytes: &[u8],
    out: &mut [f32],
    decode_sample: impl Fn([u8; N]) -> f32,
) {
    let (samples, _) = bytes.as_chunks::<N>();
    for (out, &sample) in out.iter_mut().zip(samples) {
        *out = decode_sample(sample);
    }
}

/// Why a WAV file could not be read or written.
#[derive(Debug)]
pub enum WavError {
    /// Reading, writing or seeking failed.
    Io(io::Error),
    /// The file does not start as a RIFF, RF64 or BW64 file of form `WAVE`.
    NotWave,
    /// The file ends inside its header: before its `fmt ` and `data` chunks are found, or inside
    /// one of the chunks before them.
    Truncated,
    /// No `fmt ` chunk comes before the end of the file, or before a `data` chunk of unknown size.
    MissingFormat,
    /// No `data` chunk comes before the end of the file.
    MissingData,
    /// More than 1,024 chunks come before both the `fmt ` and `data` chunks are found.
    TooManyChunks,
    /// The `fmt ` chunk holds fewer bytes than its format tag needs: this many.
    FormatTooShort(u32),
    /// The `ds64` chunk holds fewer bytes than its data size needs: this many.
    Ds64TooShort(u32),
    /// The samples are encoded in a way Magnolia does not read: this format tag (for
    /// WAVE_FORMAT_EXTENSIBLE, 0xFFFE) with this many bits per sample.
    UnsupportedFormat {
        /// The `fmt ` chunk's format tag.
        format_tag: u16,
        /// The `fmt ` chunk's bits per sample.
        bits_per_sample: u16,
    },
    /// A WAVE_FORMAT_EXTENSIBLE sub-format other than integer PCM and IEEE float.
    UnsupportedSubFormat,
    /// The channels are 0 or more than 1,024: this many.
    ChannelsOutOfRange(u16),
    /// The block align, the bytes of one frame, is not the channels times the bytes of a sample.
    BlockAlignMismatch {
        /// The block align the `fmt ` chunk gives.
        block_align: u16,
        /// The block align its channels and bits per sample give.
        expected: usize,
    },
    /// The sample rate is 0, or, in writing, so high that the bytes per second overflow their
    /// field: this many hertz.
    SampleRateOutOfRange(u32),
    /// The samples given to a writer are not whole frames.
    PartialFrame {
        /// The samples given.
        samples: usize,
        /// The writer's channels.
        channels: u16,
    },
    /// The file would pass 2^64 bytes.
    FileTooLarge,
    /// A write to the file failed before, so the writer writes nothing more; the file is left as a
    /// writer that died leaves it.
    WriterFailed,
    /// A text given to a `bext` chunk holds a character outside ASCII, or a NUL, which would end it
    /// early.
    BextFieldNotAscii {
        /// The field, as EBU Tech 3285 names it: `Description`, `Originator`,
        /// `OriginatorReference`, `OriginationDate` or `OriginationTime`.
        field: &'static str,
    },
    /// A text given to a `bext` chunk is longer than its field.
    BextFieldTooLong {
        /// The field, as EBU Tech 3285 names it.
        field: &'static str,
        /// The text's characters.
        len: usize,
        /// The field's characters.
        max: usize,
    },
    /// [`repair`] found the file unfinished, but its header has no room for its sizes where
    /// Magnolia's writer keeps it, so the file was left untouched. A file that needs 64-bit sizes,
    /// past 4 GiB or in RF64 and BW64, needs a `JUNK` or `ds64` chunk of 28 bytes right after its
    /// first 12 bytes, and no other `ds64` chunk before its data; in RF64 and BW64, its 32-bit RIFF
    /// and data sizes must say 0xFFFFFFFF.
    Unrepairable,
    /// A repaired file's header, read back, does not give the frames the repair kept: its writes
    /// did not land where they were sought, as in a file opened for appending.
    RepairNotKept,
}

impl From<io::Error> for WavError {
    /// A read that ran out of bytes found the file shorter than its header said.
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            Self::Truncated
        } else {
            Self::Io(error)
        }
    }
}

impl fmt::Display for WavError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(
                formatter,
                "the WAV file could not be read or written: {error}"
            ),
            Self::NotWave => formatter.write_str("the file is not a WAV file"),
            Self::Truncated => formatter.write_str("the WAV file ends inside its header"),
            Self::MissingFormat => {
                formatter.write_str("the WAV file has no format chunk before its data")
            }
            Self::MissingData => formatter.write_str("the WAV file has no data chunk"),
            Self::TooManyChunks => write!(
                formatter,
                "the WAV file holds more than {MAX_CHUNKS_WALKED} chunks before its format and \
                 its data"
            ),
            Self::FormatTooShort(bytes) => write!(
                formatter,
                "the WAV file's format chunk holds {bytes} bytes, too few for its format"
            ),
            Self::Ds64TooShort(bytes) => write!(
                formatter,
                "the WAV file's ds64 chunk holds {bytes} bytes, too few for its sizes"
            ),
            Self::UnsupportedFormat {
                format_tag,
                bits_per_sample,
            } => write!(
                formatter,
                "the WAV file's samples are of format {format_tag:#06x} with {bits_per_sample} \
                 bits; Magnolia reads 16-, 24- and 32-bit integers and 32-bit floats"
            ),
            Self::UnsupportedSubFormat => formatter.write_str(
                "the WAV file's samples are neither integers nor floats; Magnolia reads only those",
            ),
            Self::ChannelsOutOfRange(channels) => write!(
                formatter,
                "a WAV file holds 1 to {MAX_CHANNELS} channels, not {channels}"
            ),
            Self::BlockAlignMismatch {
                block_align,
                expected,
            } => write!(
                formatter,
                "the WAV file gives {block_align} bytes per frame where its channels and bits \
                 give {expected}"
            ),
            Self::SampleRateOutOfRange(hertz) => {
                write!(
                    formatter,
                    "a WAV file cannot have a sample rate of {hertz} Hz"
                )
            }
            Self::PartialFrame { samples, channels } => write!(
                formatter,
                "{samples} samples are not whole frames of {channels} channels"
            ),
            Self::FileTooLarge => formatter.write_str("the WAV file would pass 2^64 bytes"),
            Self::WriterFailed => formatter.write_str(
                "a write to the WAV file failed before, so nothing more is written to it",
            ),
            Self::BextFieldNotAscii { field } => write!(
                formatter,
                "the bext chunk's {field} holds a character that is not ASCII, or a NUL"
            ),
            Self::BextFieldTooLong { field, len, max } => write!(
                formatter,
                "the bext chunk's {field} holds {len} characters, more than its {max}"
            ),
            Self::Unrepairable => formatter.write_str(
                "the WAV file is unfinished, but its header has no room for its sizes where \
                 Magnolia keeps it, so it was left untouched",
            ),
            Self::RepairNotKept => formatter.write_str(
                "the repaired WAV file does not read back as repaired: its writes did not land \
                 where they were sought; is it open for appending?",
            ),
        }
    }
}

impl Error for WavError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}
