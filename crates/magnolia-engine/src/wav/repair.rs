//! Repairing a WAV file whose writer died: cut after its last whole frame, its sizes written
//! ([ADR-0030](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0030-takes-in-wav-of-unknown-length-repaired-on-recovery.md)).

use std::fs::File;
use std::io::{self, Read, Seek, Write};

use super::read::{Header, read_header};
use super::write::{DS64_BODY_BYTES, SizeFields, Sizes, write_at, write_sizes};
use super::{Magic, UNKNOWN_SIZE, WavError};

/// A file that [`repair`] can work on: read, written where it was sought, cut and synced.
/// Implemented for [`File`], which must not be opened for appending, since every write would then
/// land at its end; tests implement it for memory.
pub trait RepairableFile: Read + Write + Seek {
    /// Cuts the file to `len` bytes, or lengthens it with zeros, as [`File::set_len`] does.
    ///
    /// # Errors
    ///
    /// When the file's length cannot be changed.
    fn set_len(&mut self, len: u64) -> io::Result<()>;

    /// Makes every write and change of length before it durable, as [`File::sync_data`] does.
    ///
    /// # Errors
    ///
    /// When the file cannot be synced.
    fn sync(&mut self) -> io::Result<()>;
}

impl RepairableFile for File {
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        File::set_len(self, len)
    }

    // fdatasync, which also makes durable a change of length, as the length is needed to read the
    // data back; the other metadata, such as the times, are not needed.
    fn sync(&mut self) -> io::Result<()> {
        self.sync_data()
    }
}

/// What [`repair`] did to a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repair {
    /// The header's sizes already agreed with the file: nothing was written.
    Untouched,
    /// The file was cut after its last whole frame, and its sizes written.
    Repaired {
        /// The whole frames the file holds: every whole frame it held before.
        frames: u64,
        /// How many bytes shorter the file became: those of a frame cut short, one fewer when the
        /// pad byte after odd data takes the place of the first; 0 also when the pad byte made the
        /// file one byte longer.
        bytes_cut: u64,
    },
}

/// Where the body of a `ds64` chunk right after the file's first 12 bytes starts.
const RESERVED_DS64_BODY: u64 = 20;

/// Repairs a WAV file whose writer died, as [`WavWriter::finish`](super::WavWriter::finish) would
/// have finished it: cuts it after its last whole frame and writes its sizes. Returns what it did.
///
/// It repairs exactly the files whose sizes [`read_info`](super::read_info) reads as a writer that
/// died leaves them ([`WavInfo::length_repaired`](super::WavInfo::length_repaired)), and those
/// whose data runs past the end of the file, as when a power cut lost the end of a file whose sizes
/// were written. Every other file is left byte for byte as it was: a finished file, as
/// [`Repair::Untouched`]; one that `read_info` refuses, with its error.
///
/// The steps:
///
/// 1. The header is made to say unfinished, whatever the file's length: in RIFF, the data size
///    becomes 0xFFFFFFFF; in RF64 and BW64, the `ds64` chunk's sizes become 0.
/// 2. The file is cut after its last whole frame, and the pad byte after odd data written.
/// 3. The file is synced, so that no size written after claims bytes that did not reach the disk.
/// 4. The sizes are written as `finish` writes them: in RIFF, the RIFF size, then the data size
///    last; past 4 GiB, and always in RF64 and BW64, the file's first 48 bytes in one write, the
///    `JUNK` chunk reserved after its first 12 bytes becoming `ds64`.
/// 5. The file is synced again.
///
/// Then the header is read back, and must give the frames kept. Until the last write of step 4,
/// the file stays one that `repair` repairs: a repair cut short anywhere leaves a file that
/// `repair` takes up again, to the same bytes. Repairing a repaired file changes
/// nothing. The pad byte, never a whole frame, is not counted as data by a repair taken up.
///
/// It must not run while a writer still writes the file: recovery holds the recording folder's lock
/// for that (ADR-0030). A file whose sizes are both 0 counts every byte after its data's start as
/// data, chunks that follow it included, since nothing says where its data ends. Storage is assumed
/// to write a sector whole: a 4-byte size torn by a power cut in the middle of its write can leave a
/// file that reads as finished but short.
/// # Errors
///
/// What `read_info` returns for a file it refuses, nothing written; [`WavError::Unrepairable`],
/// nothing written, for a file without room for its sizes; [`WavError::Io`] when a read, a write, a
/// cut or a sync fails, which leaves a file that `repair` takes up again;
/// [`WavError::RepairNotKept`] when the header read back does not say what was written.
pub fn repair<F: RepairableFile>(file: &mut F) -> Result<Repair, WavError> {
    repair_with_riff_size_limit(file, u64::from(u32::MAX))
}

/// Repairs `file` as [`repair`] does, writing RF64 for a RIFF file whose RIFF size passes
/// `riff_size_limit`; only tests lower the limit, to reach RF64 without 4 GiB.
pub(super) fn repair_with_riff_size_limit<F: RepairableFile>(
    file: &mut F,
    riff_size_limit: u64,
) -> Result<Repair, WavError> {
    let header = read_header(file)?;
    let Some(plan) = Plan::new(&header, riff_size_limit)? else {
        return Ok(Repair::Untouched);
    };
    if let Some((position, bytes)) = plan.unfinished_mark {
        write_at(file, position, bytes)?;
    }
    if header.len != plan.len {
        file.set_len(plan.len)?;
    }
    // The byte after odd data may be the first of a frame cut short, which the pad byte replaces.
    if plan.pad {
        write_at(file, plan.data_end, &[0])?;
    }
    file.sync()?;
    write_sizes(file, 0, plan.data_size_at, plan.sizes, plan.fields)?;
    file.sync()?;
    // Reading the header back catches a file whose writes did not land where they were sought,
    // such as one opened for appending, which would otherwise be reported repaired.
    let repaired = read_header(file)?.info();
    if repaired.length_repaired || repaired.frames != plan.sizes.frames {
        return Err(WavError::RepairNotKept);
    }
    Ok(Repair::Repaired {
        frames: plan.sizes.frames,
        bytes_cut: header.len.saturating_sub(plan.len),
    })
}

/// What repairing a file takes, worked out before anything is written.
struct Plan {
    /// The write that makes the header say unfinished whatever the file's length, where it does not
    /// yet: where, and what.
    unfinished_mark: Option<(u64, &'static [u8])>,
    /// Where the last whole frame ends.
    data_end: u64,
    /// Whether the data is odd, so that a pad byte follows it.
    pad: bool,
    /// The file's length once repaired.
    len: u64,
    /// Where the data's 32-bit size lies.
    data_size_at: u64,
    sizes: Sizes,
    fields: SizeFields,
}

/// The data size that says unfinished in RIFF.
const UNKNOWN_SIZE_BYTES: &[u8] = &UNKNOWN_SIZE.to_le_bytes();
/// The `ds64` sizes, the RIFF's and the data's, that say unfinished in RF64 and BW64.
const UNFINISHED_DS64_SIZES: &[u8] = &[0; 16];

impl Plan {
    /// What repairing the file whose header is `header` takes, or `None` when its sizes agree with
    /// it.
    ///
    /// # Errors
    ///
    /// [`WavError::Unrepairable`] when the header has no room for the sizes.
    fn new(header: &Header, riff_size_limit: u64) -> Result<Option<Self>, WavError> {
        let available = header.len.saturating_sub(header.data.offset);
        if header
            .data
            .declared
            .is_some_and(|declared| declared <= available)
        {
            return Ok(None);
        }
        let frame_bytes = u64::from(header.format.block_align);
        let frames = available.checked_div(frame_bytes).unwrap_or(0);
        // Whole frames take no more than the bytes available, so neither result saturates.
        let data = frames.saturating_mul(frame_bytes);
        let data_end = header.data.offset.saturating_add(data);
        let pad = (data & 1) == 1;
        let len = data_end
            .checked_add(u64::from(pad))
            .ok_or(WavError::FileTooLarge)?;
        let sizes = Sizes {
            // The bytes after the RIFF's size field, which ends 8 bytes into the file.
            riff: len.saturating_sub(8),
            data,
            frames,
        };
        let fields = SizeFields::choose(header.magic, sizes, riff_size_limit);
        // The data chunk's size field lies in the 4 bytes before its body, which starts 20 bytes
        // into the file at the earliest.
        let data_size_at = header.data.offset.saturating_sub(4);
        Ok(Some(Self {
            unfinished_mark: unfinished_mark(header, fields, data_size_at)?,
            data_end,
            pad,
            len,
            data_size_at,
            sizes,
            fields,
        }))
    }
}

/// The write that makes `header` say unfinished whatever the file's length, or `None` when it
/// already does: so a repair cut short leaves a file that reads as unfinished, wherever the cut
/// fell.
///
/// # Errors
///
/// [`WavError::Unrepairable`] when the header has no room for the sizes that `fields` places, or,
/// in RF64 and BW64, holds them elsewhere than Magnolia's writer does.
fn unfinished_mark(
    header: &Header,
    fields: SizeFields,
    data_size_at: u64,
) -> Result<Option<(u64, &'static [u8])>, WavError> {
    if matches!(fields, SizeFields::Ds64 { .. }) {
        // The ds64 chunk written replaces the chunk reserved for it, of the same size, and no
        // other ds64 chunk may come after it to be read in its place.
        let reserved = matches!(
            header.first_chunk,
            Some((id, DS64_BODY_BYTES)) if &id == b"JUNK" || &id == b"ds64"
        );
        let no_other_ds64 = header
            .last_ds64
            .is_none_or(|body| body == RESERVED_DS64_BODY);
        if !(reserved && no_other_ds64) {
            return Err(WavError::Unrepairable);
        }
    }
    match header.magic {
        Magic::Riff => {
            Ok((header.data.size_field != UNKNOWN_SIZE)
                .then_some((data_size_at, UNKNOWN_SIZE_BYTES)))
        }
        Magic::Rf64 | Magic::Bw64 => {
            // The 32-bit sizes must point to ds64, where the sizes are written. ds64 sizes of 0
            // say unfinished, as does a JUNK chunk in its place.
            if header.riff_size != UNKNOWN_SIZE || header.data.size_field != UNKNOWN_SIZE {
                return Err(WavError::Unrepairable);
            }
            Ok(header
                .ds64
                .filter(|sizes| sizes.riff_size != 0 || sizes.data_size != 0)
                .map(|_| (RESERVED_DS64_BODY, UNFINISHED_DS64_SIZES)))
        }
    }
}
