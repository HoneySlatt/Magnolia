//! Files shaped like WAV files, built from a fuzzer's input, which the WAV fuzz targets share.
//!
//! From an empty corpus, as in CI, raw bytes seldom grow a `fmt ` chunk within seconds. So most
//! inputs build a file shaped like a WAV file, whose every id, size and field may still be
//! arbitrary, cut short or lying.

use libfuzzer_sys::arbitrary::{Arbitrary, Result, Unstructured};

/// Builds a file of up to eight chunks: mostly the ids, sizes and fields of a WAV file, rarely
/// arbitrary ones; a pad byte sometimes missing; the whole sometimes cut short. An input that runs
/// out ends the file early.
pub fn build_file(input: &mut Unstructured<'_>, file: &mut Vec<u8>) -> Result<()> {
    file.extend(pick(input, &[*b"RIFF", *b"RF64", *b"BW64"])?);
    file.extend(input.arbitrary::<u32>()?.to_le_bytes());
    file.extend(pick(input, &[*b"WAVE"])?);
    for _ in 0..input.int_in_range(0..=8)? {
        let id = pick(
            input,
            &[*b"fmt ", *b"data", *b"ds64", *b"bext", *b"LIST", *b"JUNK"],
        )?;
        let body = match &id {
            b"fmt " => format_body(input)?,
            b"ds64" => ds64_body(input)?,
            b"bext" => bext_body(input)?,
            _ => {
                let len = input.int_in_range(0..=512)?.min(input.len());
                input.bytes(len)?.to_vec()
            }
        };
        let size = match input.int_in_range(0..=7)? {
            0 => 0,
            1 => u32::MAX,
            2 => input.arbitrary()?,
            _ => u32::try_from(body.len()).expect("a body of at most 666 bytes"),
        };
        file.extend(id);
        file.extend(size.to_le_bytes());
        file.extend(&body);
        if body.len() % 2 == 1 && input.ratio(7, 8)? {
            file.push(0);
        }
    }
    if input.ratio(1, 8)? {
        file.truncate(input.int_in_range(0..=file.len())?);
    }
    Ok(())
}

/// One of `choices`, or rarely four arbitrary bytes.
fn pick(input: &mut Unstructured<'_>, choices: &[[u8; 4]]) -> Result<[u8; 4]> {
    if input.ratio(1, 16)? {
        input.arbitrary()
    } else {
        input.choose(choices).copied()
    }
}

/// A `fmt ` chunk's body, mostly valid, WAVE_FORMAT_EXTENSIBLE's extension included.
fn format_body(input: &mut Unstructured<'_>) -> Result<Vec<u8>> {
    let format_tag: u16 = often(input, |input| input.choose(&[1, 3, 0xFFFE]).copied())?;
    let channels: u16 = often(input, |input| input.int_in_range(1..=8))?;
    let sample_rate: u32 = often(input, |input| input.choose(&[44_100, 48_000]).copied())?;
    let bits: u16 = often(input, |input| input.choose(&[16, 24, 32]).copied())?;
    let block_align: u16 = often(input, |_| Ok(channels.wrapping_mul(bits / 8)))?;
    let mut body = [
        format_tag.to_le_bytes().as_slice(),
        &channels.to_le_bytes(),
        &sample_rate.to_le_bytes(),
        &input.arbitrary::<u32>()?.to_le_bytes(),
        &block_align.to_le_bytes(),
        &bits.to_le_bytes(),
    ]
    .concat();
    if format_tag == 0xFFFE || input.ratio(1, 8)? {
        let sub_format_tag: u16 = often(input, |input| input.choose(&[1, 3]).copied())?;
        body.extend(22_u16.to_le_bytes());
        body.extend(input.arbitrary::<[u8; 6]>()?);
        body.extend(sub_format_tag.to_le_bytes());
        body.extend([
            0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
        ]);
    }
    cut_sometimes(input, &mut body)?;
    Ok(body)
}

/// A `ds64` chunk's body: the RIFF's size, a data size mostly within a fuzzer's input, a sample
/// count and a table's length.
fn ds64_body(input: &mut Unstructured<'_>) -> Result<Vec<u8>> {
    let data_size: u64 = often(input, |input| input.int_in_range(0..=4_096))?;
    let mut body = [
        input.arbitrary::<u64>()?.to_le_bytes().as_slice(),
        &data_size.to_le_bytes(),
        &input.arbitrary::<u64>()?.to_le_bytes(),
        &input.arbitrary::<u32>()?.to_le_bytes(),
    ]
    .concat();
    cut_sometimes(input, &mut body)?;
    Ok(body)
}

/// The offsets of a `bext` chunk's originator and time reference, and the bytes of its fixed part
/// (EBU Tech 3285 v2).
const BEXT_ORIGINATOR_AT: usize = 256;
const BEXT_TIME_REFERENCE_AT: usize = 338;
const BEXT_BYTES: usize = 602;

/// A `bext` chunk's body: its fixed part, zeros but for an originator of arbitrary bytes and an
/// arbitrary time reference, sometimes followed by a coding history, sometimes cut short.
fn bext_body(input: &mut Unstructured<'_>) -> Result<Vec<u8>> {
    let mut body = vec![0; BEXT_BYTES];
    let originator_len = input.int_in_range(0..=32)?.min(input.len());
    body[BEXT_ORIGINATOR_AT..BEXT_ORIGINATOR_AT + originator_len]
        .copy_from_slice(input.bytes(originator_len)?);
    body[BEXT_TIME_REFERENCE_AT..BEXT_TIME_REFERENCE_AT + 8]
        .copy_from_slice(&input.arbitrary::<u64>()?.to_le_bytes());
    if input.ratio(1, 4)? {
        let history_len = input.int_in_range(0..=64)?.min(input.len());
        body.extend(input.bytes(history_len)?);
    }
    cut_sometimes(input, &mut body)?;
    Ok(body)
}

/// What `likely` gives, fifteen times in sixteen; an arbitrary value otherwise.
fn often<'a, T: Arbitrary<'a>>(
    input: &mut Unstructured<'a>,
    likely: impl FnOnce(&mut Unstructured<'a>) -> Result<T>,
) -> Result<T> {
    if input.ratio(1, 16)? {
        input.arbitrary()
    } else {
        likely(input)
    }
}

fn cut_sometimes(input: &mut Unstructured<'_>, body: &mut Vec<u8>) -> Result<()> {
    if input.ratio(1, 16)? {
        body.truncate(input.int_in_range(0..=body.len())?);
    }
    Ok(())
}
