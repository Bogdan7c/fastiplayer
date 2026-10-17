//! Настоящие кадры ffmpeg (`tests/fixtures`, см. README.md): свойства из ffprobe.

use std::path::PathBuf;

use ac3_syncframe::{
    AccessUnit, AccessUnitScan, StreamContinuation, SyncframeCodec, SyncframeSearch,
    locate_syncframe, scan_access_unit,
};

/// Ожидаемые свойства фикстуры, снятые `ffprobe` (см. README.md).
struct FixtureExpectation {
    file: &'static str,
    codec: SyncframeCodec,
    sample_rate_hz: u32,
    channels: u32,
    frame_lengths: &'static [usize],
}

const FIXTURES: [FixtureExpectation; 5] = [
    FixtureExpectation {
        file: "stereo-48k-192k.ac3",
        codec: SyncframeCodec::Ac3,
        sample_rate_hz: 48_000,
        channels: 2,
        frame_lengths: &[768],
    },
    FixtureExpectation {
        file: "5p1-44k1-448k.ac3",
        codec: SyncframeCodec::Ac3,
        sample_rate_hz: 44_100,
        channels: 6,
        frame_lengths: &[1950, 1952],
    },
    FixtureExpectation {
        file: "mono-32k-96k.ac3",
        codec: SyncframeCodec::Ac3,
        sample_rate_hz: 32_000,
        channels: 1,
        frame_lengths: &[576],
    },
    FixtureExpectation {
        file: "5p1-48k-448k.eac3",
        codec: SyncframeCodec::EnhancedAc3,
        sample_rate_hz: 48_000,
        channels: 6,
        frame_lengths: &[1792],
    },
    FixtureExpectation {
        file: "stereo-44k1-160k.eac3",
        codec: SyncframeCodec::EnhancedAc3,
        sample_rate_hz: 44_100,
        channels: 2,
        frame_lengths: &[696, 698],
    },
];

fn fixture_bytes(file: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(file);
    std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// Делит весь буфер на access unit-ы, как это сделал бы demuxer сырого файла.
fn split_whole_stream(bytes: &[u8]) -> Vec<AccessUnit> {
    let mut access_units = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        match scan_access_unit(&bytes[cursor..], StreamContinuation::EndOfStream) {
            Ok(AccessUnitScan::Complete(access_unit)) => {
                cursor += access_unit.total_length_bytes();
                access_units.push(access_unit);
            }
            other => panic!("offset {cursor}: {other:?}"),
        }
    }
    access_units
}

#[test]
fn ffmpeg_frames_match_ffprobe_properties_and_tile_the_file() {
    for expectation in &FIXTURES {
        let bytes = fixture_bytes(expectation.file);
        let access_units = split_whole_stream(&bytes);
        assert!(access_units.len() >= 3, "{}", expectation.file);
        let total: usize = access_units
            .iter()
            .map(AccessUnit::total_length_bytes)
            .sum();
        assert_eq!(total, bytes.len(), "{}", expectation.file);
        for access_unit in &access_units {
            let header = access_unit.primary_header();
            assert_eq!(header.codec(), expectation.codec, "{}", expectation.file);
            assert_eq!(access_unit.codec(), expectation.codec);
            assert_eq!(header.sample_rate_hz(), expectation.sample_rate_hz);
            assert_eq!(
                access_unit.presentation_channel_count(),
                expectation.channels
            );
            assert_eq!(access_unit.samples_per_frame(), 1536);
            assert_eq!(access_unit.syncframe_count(), 1);
            assert!(
                expectation
                    .frame_lengths
                    .contains(&access_unit.total_length_bytes()),
                "{}: unexpected frame length {}",
                expectation.file,
                access_unit.total_length_bytes()
            );
        }
    }
}

/// Потоковый потребитель: данные приходят кусками по 100 байт, перед первым
/// кадром мусор. Результат обязан совпасть с разбором целого файла.
#[test]
fn chunked_stream_with_leading_garbage_yields_same_access_units() {
    for expectation in &FIXTURES {
        let file_bytes = fixture_bytes(expectation.file);
        let expected = split_whole_stream(&file_bytes);
        let mut input = vec![0x0b, 0x00, 0x77, 0x0b];
        input.extend_from_slice(&file_bytes);

        let mut buffer: Vec<u8> = Vec::new();
        let mut synchronized = false;
        let mut produced = Vec::new();
        let chunks: Vec<&[u8]> = input.chunks(100).collect();
        for (index, chunk) in chunks.iter().enumerate() {
            buffer.extend_from_slice(chunk);
            let continuation = if index + 1 == chunks.len() {
                StreamContinuation::EndOfStream
            } else {
                StreamContinuation::MoreBytesMayFollow
            };
            if !synchronized {
                match locate_syncframe(&buffer) {
                    SyncframeSearch::Found { offset, .. } => {
                        buffer.drain(..offset);
                        synchronized = true;
                    }
                    SyncframeSearch::NeedMoreData {
                        discardable_prefix_bytes,
                    } => {
                        buffer.drain(..discardable_prefix_bytes);
                        continue;
                    }
                }
            }
            while let AccessUnitScan::Complete(access_unit) =
                scan_access_unit(&buffer, continuation).expect("valid stream")
            {
                buffer.drain(..access_unit.total_length_bytes());
                produced.push(access_unit);
                if buffer.is_empty() {
                    break;
                }
            }
        }
        assert!(buffer.is_empty(), "{}: leftover bytes", expectation.file);
        assert_eq!(produced, expected, "{}", expectation.file);
    }
}
