//! The WAV reader and writer through their public interface: a round trip of every format, files
//! built by hand for the cases fuzzing finds only by luck (pad bytes, sizes a writer that died
//! left, chunks after the data, WAVE_FORMAT_EXTENSIBLE, RF64 and BW64), and the plain errors of
//! malformed headers. The writer's RF64 path is tested inside the module (`src/wav/tests.rs`).

use std::io::{self, BufReader, Cursor, Seek, SeekFrom, Write};

use magnolia_engine::wav::{self, SampleFormat, WavError, WavInfo, WavWriter};

const FORMATS: [SampleFormat; 4] = [
    SampleFormat::Int16,
    SampleFormat::Int24,
    SampleFormat::Int32,
    SampleFormat::Float32,
];

// Building files by hand.

/// A chunk: its id, the size of its body, its body, and a pad byte after an odd body.
fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let size = u32::try_from(body.len()).expect("a small chunk");
    chunk_with_size(id, size, body)
}

/// A chunk whose size field says `size`, whatever its body holds.
fn chunk_with_size(id: &[u8; 4], size: u32, body: &[u8]) -> Vec<u8> {
    let mut chunk = [id.as_slice(), &size.to_le_bytes(), body].concat();
    if body.len() % 2 == 1 {
        chunk.push(0);
    }
    chunk
}

/// A file of form WAVE: `id` (RIFF, RF64 or BW64), its size, then the chunks.
fn file(id: &[u8; 4], chunks: &[Vec<u8>]) -> Vec<u8> {
    let body = chunks.concat();
    let size = u32::try_from(body.len() + 4).expect("a small file");
    [id.as_slice(), &size.to_le_bytes(), b"WAVE", &body].concat()
}

/// The file with its RIFF size field set to `size`.
fn with_riff_size(size: u32, mut file: Vec<u8>) -> Vec<u8> {
    file[4..8].copy_from_slice(&size.to_le_bytes());
    file
}

/// WAVEFORMATEX's 16 bytes, at 48 kHz.
fn format_fields(format_tag: u16, channels: u16, block_align: u16, bits: u16) -> Vec<u8> {
    let rate: u32 = 48_000;
    [
        format_tag.to_le_bytes().as_slice(),
        &channels.to_le_bytes(),
        &rate.to_le_bytes(),
        &(rate * u32::from(block_align)).to_le_bytes(),
        &block_align.to_le_bytes(),
        &bits.to_le_bytes(),
    ]
    .concat()
}

/// A `fmt ` chunk of format tag 1 (PCM) or 3 (float), its block align what its channels need.
fn simple_format(format_tag: u16, channels: u16, bits: u16) -> Vec<u8> {
    chunk(
        b"fmt ",
        &format_fields(format_tag, channels, channels * bits / 8, bits),
    )
}

/// A WAVE_FORMAT_EXTENSIBLE `fmt ` chunk of 40 bytes, its sub-format's GUID made from a format
/// tag, as Microsoft's are: 1 for integer PCM, 3 for IEEE float.
fn extensible_format(channels: u16, bits: u16, valid_bits: u16, sub_format_tag: u16) -> Vec<u8> {
    let guid = [
        sub_format_tag.to_le_bytes().as_slice(),
        &[
            0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
        ],
    ]
    .concat();
    let body = [
        format_fields(0xFFFE, channels, channels * bits / 8, bits).as_slice(),
        &22_u16.to_le_bytes(),
        &valid_bits.to_le_bytes(),
        &0x3_u32.to_le_bytes(),
        &guid,
    ]
    .concat();
    chunk(b"fmt ", &body)
}

/// A `ds64` chunk: the RIFF's and the data's 64-bit sizes, a sample count, an empty table.
fn ds64(riff_size: u64, data_size: u64, frames: u64) -> Vec<u8> {
    let body = [
        riff_size.to_le_bytes().as_slice(),
        &data_size.to_le_bytes(),
        &frames.to_le_bytes(),
        &0_u32.to_le_bytes(),
    ]
    .concat();
    chunk(b"ds64", &body)
}

fn int16_bytes(samples: &[i16]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect()
}

/// `count` 16-bit samples, each its own index: distinct, so a misplaced frame shows.
fn int16_ramp(count: i16) -> Vec<i16> {
    (0..count).collect()
}

fn read(file: &[u8]) -> Result<WavInfo, WavError> {
    wav::read_info(&mut Cursor::new(file))
}

/// The samples the data of `info` holds, decoded.
fn decoded(file: &[u8], info: &WavInfo) -> Vec<f32> {
    let start = usize::try_from(info.data_offset).expect("a small file");
    let frames = usize::try_from(info.frames).expect("a small file");
    let channels = usize::from(info.channels);
    let mut samples = vec![0.0; frames * channels];
    let decoded_frames = wav::decode(
        info.sample_format,
        info.channels,
        &file[start..],
        &mut samples,
    );
    assert_eq!(decoded_frames, frames, "every frame of the data decodes");
    samples
}

fn int16_as_f32(samples: &[i16]) -> Vec<f32> {
    samples
        .iter()
        .map(|&sample| f32::from(sample) / 32_768.0)
        .collect()
}

fn u32_at(file: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(file[at..at + 4].try_into().expect("four bytes"))
}

// The writer and the reader together.

/// A deterministic stream of pseudo-random numbers (xorshift64).
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// `count` samples that `format` holds exactly, its extremes first; for floats, any bits at all,
/// NaNs, infinities and negative zero included.
fn exact_signal(format: SampleFormat, count: usize) -> Vec<f32> {
    let mut noise = Noise(0x9E37_79B9_7F4A_7C15);
    let mut signal = match format {
        SampleFormat::Int16 => vec![-1.0, 32_767.0 / 32_768.0, 0.0],
        SampleFormat::Int24 => vec![-1.0, 8_388_607.0 / 8_388_608.0, 0.0],
        // An f32 holds 24 significant bits: the largest 32-bit integer it holds exactly is
        // 2^31 - 256.
        SampleFormat::Int32 => vec![-1.0, 2_147_483_392.0 / 2_147_483_648.0, 0.0],
        SampleFormat::Float32 => vec![-0.0, f32::INFINITY, f32::NAN, f32::MIN_POSITIVE / 2.0],
    };
    while signal.len() < count {
        let bits = noise.next();
        signal.push(match format {
            SampleFormat::Int16 => f32::from(bits as i16) / 32_768.0,
            // The top 24 bits of a random i32: a random 24-bit integer, scaled by 256.
            SampleFormat::Int24 | SampleFormat::Int32 => {
                ((bits as i32) & !0xFF) as f32 / 2_147_483_648.0
            }
            SampleFormat::Float32 => f32::from_bits(bits as u32),
        });
    }
    signal
}

#[test]
fn every_format_and_channel_count_round_trips_bit_for_bit() {
    // 1,001 frames: an odd count, so 24-bit mono data needs a pad byte; and at 6 channels, more
    // samples than the writer encodes at once.
    let frames = 1_001;
    for format in FORMATS {
        for channels in [1_u16, 2, 6] {
            let signal = exact_signal(format, frames * usize::from(channels));
            let mut writer = WavWriter::new(Cursor::new(Vec::new()), format, channels, 44_100)
                .expect("a writer");
            // In two calls, to append.
            let (first, second) = signal.split_at(300 * usize::from(channels));
            writer.write_frames(first).expect("written");
            writer.write_frames(second).expect("written");
            let file = writer.finish().expect("finished").into_inner();

            let case = format!("{format:?}, {channels} channels");
            let info = read(&file).expect(&case);
            let data_offset = if format == SampleFormat::Float32 {
                82
            } else {
                80
            };
            assert_eq!(
                info,
                WavInfo {
                    sample_format: format,
                    channels,
                    sample_rate: 44_100,
                    frames: frames as u64,
                    data_offset,
                    length_repaired: false,
                },
                "{case}"
            );
            let data_bytes = frames * usize::from(channels) * format.bytes_per_sample();
            let pad = data_bytes % 2;
            assert_eq!(
                file.len(),
                data_offset as usize + data_bytes + pad,
                "{case}"
            );
            assert_eq!(&file[0..4], b"RIFF", "{case}");
            assert_eq!(u32_at(&file, 4) as usize, file.len() - 8, "{case}");
            assert_eq!(&file[12..16], b"JUNK", "{case}");
            assert_eq!(u32_at(&file, 16), 28, "{case}");
            assert_eq!(
                u32_at(&file, data_offset as usize - 4) as usize,
                data_bytes,
                "{case}"
            );

            let bits = |samples: &[f32]| samples.iter().map(|s| s.to_bits()).collect::<Vec<_>>();
            assert_eq!(bits(&decoded(&file, &info)), bits(&signal), "{case}");
        }
    }
}

#[test]
fn integers_are_rounded_to_the_nearest_and_clamped() {
    let step16 = 1.0 / 32_768.0;
    let step24 = 1.0 / 8_388_608.0;
    let samples = [
        0.5 * step16,
        0.49 * step16,
        -0.5 * step16,
        1.5 * step24,
        1.0,
        -1.0,
        2.0,
        -2.0,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NAN,
    ];
    // What each sample becomes, in units of each format's step.
    let int16: [i64; 11] = [
        1, 0, -1, 0, 32_767, -32_768, 32_767, -32_768, 32_767, -32_768, 0,
    ];
    let int24: [i64; 11] = [
        128, 125, -128, 2, 8_388_607, -8_388_608, 8_388_607, -8_388_608, 8_388_607, -8_388_608, 0,
    ];
    let int32_max = i64::from(i32::MAX);
    let int32_min = i64::from(i32::MIN);
    let int32: [i64; 11] = [
        32_768, 32_113, -32_768, 384, int32_max, int32_min, int32_max, int32_min, int32_max,
        int32_min, 0,
    ];
    for (format, expected) in [
        (SampleFormat::Int16, int16),
        (SampleFormat::Int24, int24),
        (SampleFormat::Int32, int32),
    ] {
        let mut writer =
            WavWriter::new(Cursor::new(Vec::new()), format, 1, 48_000).expect("a writer");
        writer.write_frames(&samples).expect("written");
        let file = writer.finish().expect("finished").into_inner();
        let width = format.bytes_per_sample();
        let written: Vec<i64> = file[80..80 + samples.len() * width]
            .chunks_exact(width)
            .map(|bytes| {
                // Sign-extends the little-endian integer from its top byte.
                let mut value = i64::from(bytes[width - 1] as i8);
                for &byte in bytes[..width - 1].iter().rev() {
                    value = (value << 8) | i64::from(byte);
                }
                value
            })
            .collect();
        assert_eq!(written, expected, "{format:?}");
    }
}

#[test]
fn a_writer_that_died_leaves_a_file_read_to_its_end() {
    let mut file = Cursor::new(Vec::new());
    let signal = int16_as_f32(&int16_ramp(10));
    {
        let mut writer =
            WavWriter::new(&mut file, SampleFormat::Int16, 2, 48_000).expect("a writer");
        writer.write_frames(&signal).expect("written");
        // Dropped without `finish`, as a crash would leave it.
    }
    let file = file.into_inner();
    assert_eq!(u32_at(&file, 4), 0, "the RIFF size, left at 0");
    assert_eq!(u32_at(&file, 76), 0, "the data size, left at 0");
    let info = read(&file).expect("read");
    assert_eq!(info.frames, 5);
    assert!(info.length_repaired);
    assert_eq!(decoded(&file, &info), signal);
}

#[test]
fn a_finished_file_without_frames_reads_0_frames_even_with_a_chunk_appended() {
    let writer = WavWriter::new(Cursor::new(Vec::new()), SampleFormat::Float32, 1, 48_000)
        .expect("a writer");
    let mut file = writer.finish().expect("finished").into_inner();
    assert_eq!(file.len(), 82);
    let info = read(&file).expect("read");
    assert_eq!((info.frames, info.length_repaired), (0, false));

    // A tool that tags the file appends a chunk, and grows the RIFF size to hold it.
    file.extend(chunk(b"LIST", b"INFOabcdefgh"));
    let riff_size = u32::try_from(file.len() - 8).expect("a small file");
    let file = with_riff_size(riff_size, file);
    let info = read(&file).expect("read");
    assert_eq!(
        (info.frames, info.length_repaired),
        (0, false),
        "the LIST chunk is not audio"
    );
}

/// A file that takes part of one write and fails the next, as a full disk does.
#[derive(Debug)]
struct FailingFile {
    file: Cursor<Vec<u8>>,
    writes: usize,
    /// The write that takes only part of its bytes; the one after it fails.
    partial_write: usize,
}

impl Write for FailingFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        if self.writes == self.partial_write {
            return self.file.write(&bytes[..bytes.len() / 2 + 1]);
        }
        if self.writes == self.partial_write + 1 {
            return Err(io::Error::other("the disk is full"));
        }
        self.file.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for FailingFile {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.file.seek(to)
    }
}

#[test]
fn after_a_failed_write_the_writer_refuses_to_go_on() {
    let mut target = FailingFile {
        file: Cursor::new(Vec::new()),
        writes: 0,
        partial_write: 3,
    };
    // The header is the first write, these frames the second.
    let mut writer = WavWriter::new(&mut target, SampleFormat::Int16, 2, 48_000).expect("a writer");
    writer.write_frames(&[0.25; 4]).expect("written");
    // The third write takes 5 of these frames' 8 bytes, and the fourth fails.
    let error = writer.write_frames(&[0.5; 4]).expect_err("a full disk");
    assert!(matches!(error, WavError::Io(_)), "{error:?}");
    let error = writer.write_frames(&[0.75; 4]).expect_err("refused");
    assert!(matches!(error, WavError::WriterFailed), "{error:?}");
    let error = writer.finish().expect_err("refused");
    assert!(matches!(error, WavError::WriterFailed), "{error:?}");

    // The file is left as a writer that died leaves it: its whole frames read to the end.
    let file = target.file.into_inner();
    assert_eq!(file.len(), 80 + 8 + 5);
    let info = read(&file).expect("read");
    assert!(info.length_repaired);
    assert_eq!(decoded(&file, &info), [0.25, 0.25, 0.25, 0.25, 0.5, 0.5]);
}

#[test]
fn the_writer_writes_from_its_position_and_ends_at_the_end_of_the_file() {
    let mut cursor = Cursor::new(b"before".to_vec());
    cursor.seek(std::io::SeekFrom::End(0)).expect("seek");
    let mut writer = WavWriter::new(cursor, SampleFormat::Float32, 1, 48_000).expect("a writer");
    writer.write_frames(&[0.25, -0.5, 1.5]).expect("written");
    let cursor = writer.finish().expect("finished");
    assert_eq!(cursor.position(), cursor.get_ref().len() as u64);
    let file = cursor.into_inner();
    assert_eq!(&file[..6], b"before");
    let wav = &file[6..];
    let info = read(wav).expect("read");
    assert_eq!(decoded(wav, &info), [0.25, -0.5, 1.5]);
}

#[test]
fn the_writer_refuses_partial_frames_and_writes_nothing_of_them() {
    let mut file = Cursor::new(Vec::new());
    let mut writer = WavWriter::new(&mut file, SampleFormat::Int16, 3, 48_000).expect("a writer");
    let error = writer
        .write_frames(&[0.0; 7])
        .expect_err("7 samples of 3 channels");
    assert!(matches!(
        error,
        WavError::PartialFrame {
            samples: 7,
            channels: 3
        }
    ));
    writer.finish().expect("finished");
    assert_eq!(read(file.get_ref()).expect("read").frames, 0);
}

#[test]
fn the_writer_refuses_channels_and_sample_rates_a_file_cannot_hold() {
    let new = |channels, rate| {
        WavWriter::new(Cursor::new(Vec::new()), SampleFormat::Int32, channels, rate)
    };
    assert!(matches!(
        new(0, 48_000),
        Err(WavError::ChannelsOutOfRange(0))
    ));
    assert!(matches!(
        new(1_025, 48_000),
        Err(WavError::ChannelsOutOfRange(1_025))
    ));
    assert!(new(1_024, 48_000).is_ok());
    assert!(matches!(new(2, 0), Err(WavError::SampleRateOutOfRange(0))));
    // 1,024 channels of 4 bytes overflow 32 bits of bytes per second past 1,048,575 Hz.
    assert!(new(1_024, 1_048_575).is_ok());
    assert!(matches!(
        new(1_024, 1_048_576),
        Err(WavError::SampleRateOutOfRange(1_048_576))
    ));
}

// Files built by hand.

#[test]
fn a_pad_byte_follows_an_odd_sized_unknown_chunk_before_the_format() {
    let samples = int16_ramp(8);
    let file = file(
        b"RIFF",
        &[
            chunk(b"odd ", b"abc"),
            simple_format(1, 2, 16),
            chunk(b"data", &int16_bytes(&samples)),
        ],
    );
    let info = read(&file).expect("read");
    assert_eq!(info.data_offset, 12 + 12 + 24 + 8);
    assert_eq!(info.frames, 4);
    assert_eq!(decoded(&file, &info), int16_as_f32(&samples));
}

#[test]
fn the_sizes_a_writer_that_died_leaves_read_whole_frames_to_the_end() {
    let samples = int16_ramp(14);
    // Seven stereo frames, then three bytes of a frame cut short.
    let data = [int16_bytes(&samples), vec![1, 2, 3]].concat();
    // A data size of 0 with a RIFF size of 0 or 0xFFFFFFFF; a data size of 0xFFFFFFFF with any.
    for (riff_size, data_size) in [(0, 0), (u32::MAX, 0), (0, u32::MAX), (36, u32::MAX)] {
        let mut file = with_riff_size(riff_size, file(b"RIFF", &[simple_format(1, 2, 16)]));
        file.extend(chunk_with_size(b"data", data_size, &[]));
        file.extend(&data);
        let case = format!("RIFF size {riff_size:#x}, data size {data_size:#x}");
        let info = read(&file).expect(&case);
        assert_eq!(info.frames, 7, "{case}");
        assert!(info.length_repaired, "{case}");
        assert_eq!(decoded(&file, &info), int16_as_f32(&samples));
    }
}

#[test]
fn an_empty_data_chunk_holds_no_frames_when_the_riff_size_is_known() {
    // As a tagger leaves a file without audio: its data empty, a LIST chunk after it.
    let list = chunk(b"LIST", b"INFOISFT\x08\x00\x00\x00Magnolia");
    let riff = file(
        b"RIFF",
        &[simple_format(1, 2, 16), chunk(b"data", &[]), list.clone()],
    );
    let info = read(&riff).expect("read");
    assert_eq!((info.frames, info.length_repaired), (0, false));

    // In RF64 and BW64, the RIFF size is known through ds64.
    let mut rf64 = with_riff_size(
        u32::MAX,
        file(b"RF64", &[ds64(100, 0, 0), simple_format(1, 2, 16)]),
    );
    rf64.extend(chunk_with_size(b"data", u32::MAX, &[]));
    rf64.extend(&list);
    let info = read(&rf64).expect("read");
    assert_eq!((info.frames, info.length_repaired), (0, false));
}

#[test]
fn a_data_size_past_the_end_is_cut_to_whole_frames() {
    let samples = int16_ramp(14);
    let mut file = file(b"RIFF", &[simple_format(1, 2, 16)]);
    file.extend(chunk_with_size(b"data", 1_000, &[]));
    file.extend(int16_bytes(&samples));
    file.push(9);
    let info = read(&file).expect("read");
    assert_eq!(info.frames, 7);
    assert!(!info.length_repaired);
}

#[test]
fn chunks_after_the_data_are_left_alone() {
    let samples = int16_ramp(8);
    let file = file(
        b"RIFF",
        &[
            simple_format(1, 2, 16),
            chunk(b"data", &int16_bytes(&samples)),
            chunk(b"LIST", b"INFOISFT\x05\x00\x00\x00test\x00"),
        ],
    );
    let info = read(&file).expect("read");
    assert_eq!(info.frames, 4);
    assert!(!info.length_repaired);
    assert_eq!(decoded(&file, &info), int16_as_f32(&samples));
}

#[test]
fn a_format_after_the_data_is_found_past_the_datas_pad_byte() {
    // Three mono 24-bit frames: nine bytes, odd, so a pad byte follows them.
    let data = [0x01, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x80];
    let file = file(b"RIFF", &[chunk(b"data", &data), simple_format(1, 1, 24)]);
    let info = read(&file).expect("read");
    assert_eq!(info.data_offset, 20);
    assert_eq!(info.frames, 3);
    assert_eq!(
        decoded(&file, &info),
        [1.0 / 8_388_608.0, -1.0 / 8_388_608.0, -1.0]
    );
    let mut reader = Cursor::new(&file);
    wav::read_info(&mut reader).expect("read");
    assert_eq!(reader.position(), 20, "the reader is left at the data");
}

#[test]
fn the_reader_is_left_at_the_data_through_a_buffered_reader() {
    let file = file(
        b"RIFF",
        &[
            chunk(b"bext", &[0; 602]),
            simple_format(3, 1, 32),
            chunk(b"data", &0.5_f32.to_le_bytes()),
        ],
    );
    let mut reader = BufReader::new(Cursor::new(&file));
    let info = wav::read_info(&mut reader).expect("read");
    assert_eq!(
        reader.stream_position().expect("position"),
        info.data_offset
    );
    assert_eq!(decoded(&file, &info), [0.5]);
}

#[test]
fn wave_format_extensible_reads_24_bit_integers_and_floats() {
    // Stereo 24-bit: the extremes, then one step each side of zero.
    let int24 = [
        0x00, 0x00, 0x80, 0xFF, 0xFF, 0x7F, 0x01, 0x00, 0x00, 0xFF, 0xFF, 0xFF,
    ];
    let file_24 = file(
        b"RIFF",
        &[extensible_format(2, 24, 24, 1), chunk(b"data", &int24)],
    );
    let info = read(&file_24).expect("read");
    assert_eq!(info.sample_format, SampleFormat::Int24);
    assert_eq!((info.channels, info.frames), (2, 2));
    assert_eq!(
        decoded(&file_24, &info),
        [
            -1.0,
            8_388_607.0 / 8_388_608.0,
            1.0 / 8_388_608.0,
            -1.0 / 8_388_608.0
        ]
    );

    let floats = [0.75_f32, -0.125, 3.0];
    let float_bytes: Vec<u8> = floats.iter().flat_map(|f| f.to_le_bytes()).collect();
    let file_float = file(
        b"RIFF",
        &[
            extensible_format(1, 32, 32, 3),
            chunk(b"data", &float_bytes),
        ],
    );
    let info = read(&file_float).expect("read");
    assert_eq!(info.sample_format, SampleFormat::Float32);
    assert_eq!(decoded(&file_float, &info), floats);

    // 24 valid bits in a 32-bit container read as 32-bit integers, their low byte zero.
    let file_32 = file(
        b"RIFF",
        &[
            extensible_format(1, 32, 24, 1),
            chunk(b"data", &(-256_i32).to_le_bytes()),
        ],
    );
    let info = read(&file_32).expect("read");
    assert_eq!(info.sample_format, SampleFormat::Int32);
    assert_eq!(decoded(&file_32, &info), [-1.0 / 8_388_608.0]);
}

#[test]
fn rf64_and_bw64_read_the_data_size_from_ds64() {
    let samples = int16_ramp(12);
    for id in [b"RF64", b"BW64"] {
        // Six stereo frames declared through ds64; a LIST chunk after them is not data.
        let mut file = file(id, &[ds64(0, 24, 6), simple_format(1, 2, 16)]);
        file.extend(chunk_with_size(b"data", u32::MAX, &int16_bytes(&samples)));
        file.extend(chunk(b"LIST", b"INFO"));
        let info = read(&file).expect("read");
        assert_eq!(info.frames, 6, "{id:?}");
        assert!(!info.length_repaired, "{id:?}");
        assert_eq!(decoded(&file, &info), int16_as_f32(&samples));
    }
}

#[test]
fn rf64_sizes_beyond_the_file_or_unknown_read_to_its_end() {
    let samples = int16_ramp(12);
    // ds64 declares 4 GiB of data, past the end: cut to the file's six frames.
    let mut file_large = file(b"RF64", &[ds64(0, 1 << 32, 0), simple_format(1, 2, 16)]);
    file_large.extend(chunk_with_size(b"data", u32::MAX, &int16_bytes(&samples)));
    let info = read(&file_large).expect("read");
    assert_eq!(info.frames, 6);
    assert!(!info.length_repaired);

    // ds64 sizes of 0, as a writer that died may leave them, read to the end.
    let mut file_zero = with_riff_size(
        u32::MAX,
        file(b"RF64", &[ds64(0, 0, 0), simple_format(1, 2, 16)]),
    );
    file_zero.extend(chunk_with_size(b"data", u32::MAX, &int16_bytes(&samples)));
    let info = read(&file_zero).expect("read");
    assert_eq!(info.frames, 6);
    assert!(info.length_repaired);

    // A ds64 chunk means nothing in a RIFF file: 0xFFFFFFFF there is a writer that died.
    let mut file_riff = file(b"RIFF", &[ds64(0, 4, 1), simple_format(1, 2, 16)]);
    file_riff.extend(chunk_with_size(b"data", u32::MAX, &int16_bytes(&samples)));
    let info = read(&file_riff).expect("read");
    assert_eq!(info.frames, 6);
    assert!(info.length_repaired);
}

// Plain errors.

#[test]
fn a_header_cut_short_is_a_plain_error() {
    let mut writer =
        WavWriter::new(Cursor::new(Vec::new()), SampleFormat::Int16, 2, 48_000).expect("a writer");
    writer.write_frames(&[0.5; 8]).expect("written");
    let file = writer.finish().expect("finished").into_inner();
    for len in 0..80 {
        let error = read(&file[..len]).expect_err("a header cut short");
        let expected = match len {
            // The RIFF header, the JUNK and fmt chunks, each cut short.
            0..12 | 13..48 | 49..72 | 73..80 => matches!(error, WavError::Truncated),
            // Cut between chunks, before the fmt chunk or the data chunk.
            12 | 48 => matches!(error, WavError::MissingFormat),
            72 => matches!(error, WavError::MissingData),
            _ => unreachable!(),
        };
        assert!(expected, "cut at {len} bytes: {error:?}");
    }
    // The data chunk's header whole: the file holds no frame, but is a WAV file.
    assert_eq!(read(&file[..80]).expect("read").frames, 0);
}

#[test]
fn a_file_of_another_kind_is_a_plain_error() {
    for header in [
        b"RIFX\x24\x00\x00\x00WAVE".as_slice(),
        b"RIFF\x24\x00\x00\x00AVI ",
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0d",
    ] {
        assert!(matches!(read(header), Err(WavError::NotWave)), "{header:?}");
    }
}

#[test]
fn a_format_chunk_too_short_is_a_plain_error() {
    let short = chunk(b"fmt ", &format_fields(1, 2, 4, 16)[..14]);
    let file_short = file(b"RIFF", &[short, chunk(b"data", &[0; 4])]);
    assert!(matches!(
        read(&file_short),
        Err(WavError::FormatTooShort(14))
    ));

    // WAVE_FORMAT_EXTENSIBLE needs 40 bytes; 18 hold only WAVEFORMATEX and an empty extension.
    let body = [format_fields(0xFFFE, 2, 4, 16), vec![0, 0]].concat();
    let file_extensible = file(b"RIFF", &[chunk(b"fmt ", &body), chunk(b"data", &[0; 4])]);
    assert!(matches!(
        read(&file_extensible),
        Err(WavError::FormatTooShort(18))
    ));

    let mut file_ds64 = file(
        b"RF64",
        &[chunk(b"ds64", &[0; 12]), simple_format(1, 2, 16)],
    );
    file_ds64.extend(chunk_with_size(b"data", u32::MAX, &[0; 4]));
    assert!(matches!(read(&file_ds64), Err(WavError::Ds64TooShort(12))));
}

#[test]
fn a_block_align_that_lies_is_a_plain_error() {
    for block_align in [0, 2, 6] {
        let format = chunk(b"fmt ", &format_fields(1, 2, block_align, 16));
        let file = file(b"RIFF", &[format, chunk(b"data", &[0; 12])]);
        let error = read(&file).expect_err("a block align that lies");
        assert!(
            matches!(
                error,
                WavError::BlockAlignMismatch {
                    block_align: found,
                    expected: 4
                } if found == block_align
            ),
            "{error:?}"
        );
    }
}

#[test]
fn channels_out_of_range_are_a_plain_error() {
    for channels in [0, 1_025] {
        let format = chunk(b"fmt ", &format_fields(1, channels, channels * 2, 16));
        let file = file(b"RIFF", &[format, chunk(b"data", &[0; 4])]);
        let error = read(&file).expect_err("channels out of range");
        assert!(
            matches!(error, WavError::ChannelsOutOfRange(found) if found == channels),
            "{error:?}"
        );
    }
    let file = file(
        b"RIFF",
        &[simple_format(1, 1_024, 16), chunk(b"data", &[0; 2_048])],
    );
    assert_eq!(read(&file).expect("1,024 channels").frames, 1);
}

#[test]
fn encodings_magnolia_does_not_read_are_a_plain_error() {
    for (format_tag, bits) in [
        (1, 8),
        (1, 20),
        (1, 64),
        (3, 16),
        (3, 64),
        (2, 4),
        (0x55, 0),
    ] {
        let format = chunk(
            b"fmt ",
            &format_fields(format_tag, 1, bits.max(8) / 8, bits),
        );
        let file = file(b"RIFF", &[format, chunk(b"data", &[0; 8])]);
        let error = read(&file).expect_err("an unsupported encoding");
        assert!(
            matches!(
                error,
                WavError::UnsupportedFormat { format_tag: tag, bits_per_sample }
                    if tag == format_tag && bits_per_sample == bits
            ),
            "{format_tag:#x}, {bits} bits: {error:?}"
        );
    }
    // An extensible sub-format other than PCM and float: A-law.
    let file_alaw = file(
        b"RIFF",
        &[extensible_format(1, 16, 16, 6), chunk(b"data", &[0; 2])],
    );
    assert!(matches!(
        read(&file_alaw),
        Err(WavError::UnsupportedSubFormat)
    ));
}

#[test]
fn a_sample_rate_of_0_is_a_plain_error() {
    let mut fields = format_fields(1, 1, 2, 16);
    fields[4..8].copy_from_slice(&0_u32.to_le_bytes());
    let file = file(b"RIFF", &[chunk(b"fmt ", &fields), chunk(b"data", &[0; 2])]);
    assert!(matches!(
        read(&file),
        Err(WavError::SampleRateOutOfRange(0))
    ));
}

#[test]
fn a_data_chunk_of_unknown_size_before_the_format_is_a_plain_error() {
    let mut file = file(b"RIFF", &[]);
    file.extend(chunk_with_size(b"data", u32::MAX, &[0; 8]));
    file.extend(simple_format(1, 1, 16));
    assert!(matches!(read(&file), Err(WavError::MissingFormat)));
}

#[test]
fn the_walk_stops_after_1_024_chunks_before_the_format_and_the_data() {
    let file_after = |empty_chunks: usize| {
        let mut chunks = vec![chunk(b"JUNK", &[]); empty_chunks];
        chunks.push(simple_format(1, 1, 16));
        chunks.push(chunk(b"data", &[0; 2]));
        file(b"RIFF", &chunks)
    };
    let info = read(&file_after(1_000)).expect("1,000 chunks before the format");
    assert_eq!(info.frames, 1);
    let error = read(&file_after(2_000)).expect_err("2,000 chunks before the format");
    assert!(matches!(error, WavError::TooManyChunks), "{error:?}");

    // Zeros after the format read as empty chunks, 8 bytes apart.
    let mut zeros = file(b"RIFF", &[simple_format(1, 2, 16)]);
    zeros.resize(zeros.len() + (1 << 20), 0);
    let error = read(&zeros).expect_err("a megabyte of zeros");
    assert!(matches!(error, WavError::TooManyChunks), "{error:?}");
}

#[test]
fn a_chunk_cut_short_before_the_format_is_a_plain_error() {
    let mut file = file(b"RIFF", &[]);
    file.extend(chunk_with_size(b"LIST", 1_000, &[0; 10]));
    assert!(matches!(read(&file), Err(WavError::Truncated)));
}

#[test]
fn errors_say_what_is_wrong_in_plain_words() {
    let io = WavError::from(std::io::Error::other("the disk is gone"));
    assert!(std::error::Error::source(&io).is_some());
    assert!(io.to_string().contains("the disk is gone"));
    let cut = WavError::from(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
    assert!(matches!(cut, WavError::Truncated));
    assert_eq!(
        WavError::ChannelsOutOfRange(0).to_string(),
        "a WAV file holds 1 to 1024 channels, not 0"
    );
}

// Decoding.

#[test]
fn decode_stops_at_the_whole_frames_both_sides_hold() {
    // Five stereo frames and half of a sixth.
    let bytes = [int16_bytes(&int16_ramp(10)), vec![7, 7]].concat();

    let mut room_for_more = [9.0; 14];
    assert_eq!(
        wav::decode(SampleFormat::Int16, 2, &bytes, &mut room_for_more),
        5
    );
    assert_eq!(room_for_more[..10], int16_as_f32(&int16_ramp(10))[..]);
    assert_eq!(
        room_for_more[10..],
        [9.0; 4],
        "the rest of out is left alone"
    );

    let mut room_for_less = [9.0; 7];
    assert_eq!(
        wav::decode(SampleFormat::Int16, 2, &bytes, &mut room_for_less),
        3
    );
    assert_eq!(room_for_less[..6], int16_as_f32(&int16_ramp(6))[..]);
    assert_eq!(room_for_less[6], 9.0);

    for format in FORMATS {
        assert_eq!(wav::decode(format, 0, &bytes, &mut room_for_more), 0);
        assert_eq!(wav::decode(format, 1, &[], &mut room_for_more), 0);
        assert_eq!(wav::decode(format, 1, &bytes, &mut []), 0);
    }
}

#[test]
fn decode_scales_each_integer_by_its_full_scale() {
    let mut out = [0.0; 2];
    wav::decode(
        SampleFormat::Int16,
        1,
        &int16_bytes(&[i16::MIN, i16::MAX]),
        &mut out,
    );
    assert_eq!(out, [-1.0, 32_767.0 / 32_768.0]);
    let int32 = [i32::MIN.to_le_bytes(), 1_i32.to_le_bytes()].concat();
    wav::decode(SampleFormat::Int32, 1, &int32, &mut out);
    assert_eq!(out, [-1.0, 1.0 / 2_147_483_648.0]);
}
