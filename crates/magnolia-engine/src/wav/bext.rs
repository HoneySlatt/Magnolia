//! The `bext` chunk of Broadcast WAV files (EBU Tech 3285, version 2): what a writer puts in it,
//! and the marks a reader takes from it.

use super::WavError;

/// The bytes of a `bext` chunk's fixed part, which version 2 lays out; a coding history may follow
/// it, which Magnolia neither writes nor reads.
pub(super) const BEXT_BYTES: u32 = 602;

// The fixed part's fields, in order, with their offsets from the chunk's body (Tech 3285 v2,
// section 2.3): Description at 0, Originator at 256, OriginatorReference at 288, OriginationDate
// at 320, OriginationTime at 330, TimeReference at 338 (its low 32 bits, then its high 32 bits),
// Version at 346, UMID at 348, the five loudness values at 412, then reserved bytes from 422 to
// 602.
pub(super) const DESCRIPTION_BYTES: usize = 256;
pub(super) const ORIGINATOR_BYTES: usize = 32;
pub(super) const ORIGINATOR_REFERENCE_BYTES: usize = 32;
const ORIGINATION_DATE_BYTES: usize = 10;
const ORIGINATION_TIME_BYTES: usize = 8;
/// The date and the time together, which the reader steps over.
pub(super) const ORIGINATION_BYTES: usize = ORIGINATION_DATE_BYTES + ORIGINATION_TIME_BYTES;
const UMID_BYTES: usize = 64;
const LOUDNESS_FIELDS: usize = 5;
const RESERVED_BYTES: usize = 180;

/// The version written, the one that carries the loudness values.
const VERSION: u16 = 2;
/// A loudness value that is not given: outside every value's range, so readers ignore it.
const LOUDNESS_UNUSED: u16 = 0x7FFF;

/// The `bext` chunk that [`WavWriter::with_bext`](super::WavWriter::with_bext) writes (EBU Tech
/// 3285, version 2): who made a file, and where its first frame lies.
///
/// Each text is ASCII, at most its field's length, without NUL, which would end it early; a shorter
/// text is padded with NULs. The writer sets the version to 2, the UMID to zeros, the five loudness
/// values to 0x7FFF, which says that they are not given, the reserved bytes to zeros, and writes no
/// coding history.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bext<'a> {
    /// A description of the sound: up to 256 characters.
    pub description: &'a str,
    /// Who made the file: up to 32 characters. Magnolia's takes say `Magnolia`.
    pub originator: &'a str,
    /// A reference unique among the originator's files: up to 32 characters.
    pub originator_reference: &'a str,
    /// The day the file was made, as `yyyy-mm-dd`: up to 10 characters, their form not checked.
    pub origination_date: &'a str,
    /// The time the file was made, as `hh:mm:ss`: up to 8 characters, their form not checked.
    pub origination_time: &'a str,
    /// The sample of the file's first frame, at its sample rate. Tech 3285 counts from midnight;
    /// Magnolia's takes count from the start of the timeline
    /// ([ADR-0030](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0030-takes-in-wav-of-unknown-length-repaired-on-recovery.md)).
    pub time_reference: u64,
}

impl Bext<'_> {
    /// The chunk's body, its 602 bytes.
    ///
    /// # Errors
    ///
    /// [`WavError::BextFieldNotAscii`] or [`WavError::BextFieldTooLong`] for the first text that
    /// its field cannot hold.
    pub(super) fn body(&self) -> Result<Vec<u8>, WavError> {
        Ok([
            text::<DESCRIPTION_BYTES>("Description", self.description)?.as_slice(),
            &text::<ORIGINATOR_BYTES>("Originator", self.originator)?,
            &text::<ORIGINATOR_REFERENCE_BYTES>("OriginatorReference", self.originator_reference)?,
            &text::<ORIGINATION_DATE_BYTES>("OriginationDate", self.origination_date)?,
            &text::<ORIGINATION_TIME_BYTES>("OriginationTime", self.origination_time)?,
            // The low 32 bits, then the high 32 bits, each little-endian: the u64's own bytes.
            &self.time_reference.to_le_bytes(),
            &VERSION.to_le_bytes(),
            &[0; UMID_BYTES],
            [LOUDNESS_UNUSED.to_le_bytes(); LOUDNESS_FIELDS].as_flattened(),
            &[0; RESERVED_BYTES],
        ]
        .concat())
    }
}

/// A text field of `N` bytes: `value`, then NULs.
fn text<const N: usize>(field: &'static str, value: &str) -> Result<[u8; N], WavError> {
    if !value.bytes().all(|byte| byte.is_ascii() && byte != 0) {
        return Err(WavError::BextFieldNotAscii { field });
    }
    if value.len() > N {
        return Err(WavError::BextFieldTooLong {
            field,
            len: value.len(),
            max: N,
        });
    }
    let mut bytes = [0; N];
    for (byte, value_byte) in bytes.iter_mut().zip(value.bytes()) {
        *byte = value_byte;
    }
    Ok(bytes)
}

/// The marks that [`read_info`](super::read_info) takes from a file's `bext` chunk: who made the
/// file, and where its first frame lies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BextInfo {
    /// The sample of the file's first frame, at its sample rate, as [`Bext::time_reference`] says.
    pub time_reference: u64,
    pub(super) originator: [u8; ORIGINATOR_BYTES],
    pub(super) originator_reference: [u8; ORIGINATOR_REFERENCE_BYTES],
}

impl BextInfo {
    /// Who made the file: the field's bytes up to its first NUL. Another program's may hold bytes
    /// other than ASCII, so they are given as they lie.
    pub fn originator(&self) -> &[u8] {
        until_nul(&self.originator)
    }

    /// The originator's reference of the file: the field's bytes up to its first NUL, as they lie.
    pub fn originator_reference(&self) -> &[u8] {
        until_nul(&self.originator_reference)
    }
}

/// The bytes of a text field up to its first NUL, or all of them when it is full.
fn until_nul(field: &[u8]) -> &[u8] {
    field.split(|&byte| byte == 0).next().unwrap_or(field)
}
