//! Общие фикстуры и источники для тестов сырого AC-3.

use std::io::Cursor;
use std::num::NonZeroUsize;
use std::time::Duration;

use ac3_elementary_demux::{
    Ac3ElementaryDemuxFactory, Ac3ElementaryDemuxOptions, Ac3ElementaryDemuxer,
};
use demux_api::{DemuxHints, DemuxInput, DemuxRegistry, DemuxSniffBudget, DemuxSourceExtension};
use media_core::{DemuxReadEvent, Demuxer, Packet};
use source_core::{
    ByteSource, CancellationToken, NotSeekableReason, Seekability, SourceError, SourceFingerprint,
    SourceResult, SourceValidators,
};

/// AC-3 stereo 48 кГц 192 кбит/с: 16 кадров по 768 байт (0.512 с), FL 440 / FR 1000 Гц.
pub const STEREO_AC3: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../audio/tests/fixtures/ac3/tones-stereo.ac3"
));

/// E-AC-3 5.1 48 кГц 384 кбит/с: 16 access unit-ов по 1536 байт.
pub const SURROUND_EAC3: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../audio/tests/fixtures/ac3/tones-5.1.eac3"
));

/// AC-3 5.1 44.1 кГц: 3 кадра 1950, 1952, 1950 байт (длина кадра чередуется).
pub const SURROUND_AC3_44K1: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../ac3-syncframe/tests/fixtures/5p1-44k1-448k.ac3"
));

pub const STEREO_FRAME_BYTES: usize = 768;
pub const SURROUND_EAC3_FRAME_BYTES: usize = 1536;
pub const FIXTURE_FRAMES: usize = 16;
pub const SAMPLES_PER_FRAME: u64 = 1536;

/// Время начала кадра `index` при 48 кГц.
pub fn frame_start(index: u64) -> Duration {
    Duration::from_nanos(index * SAMPLES_PER_FRAME * 1_000_000_000 / 48_000)
}

/// Seekable/non-seekable `ByteSource` в памяти.
pub struct MemoryByteSource {
    bytes: Vec<u8>,
    position: u64,
    seekable: bool,
}

impl MemoryByteSource {
    pub fn seekable(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.to_vec(),
            position: 0,
            seekable: true,
        }
    }

    pub fn forward_only(bytes: &[u8]) -> Self {
        Self {
            seekable: false,
            ..Self::seekable(bytes)
        }
    }
}

impl ByteSource for MemoryByteSource {
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }
        let start = usize::try_from(self.position).unwrap_or(usize::MAX);
        if start >= self.bytes.len() {
            return Ok(0);
        }
        let count = output.len().min(self.bytes.len() - start);
        output[..count].copy_from_slice(&self.bytes[start..start + count]);
        self.position += count as u64;
        Ok(count)
    }

    fn seek(&mut self, offset: u64) -> SourceResult<()> {
        if !self.seekable {
            return Err(SourceError::NotSeekable {
                reason: NotSeekableReason::Unknown,
            });
        }
        self.position = offset;
        Ok(())
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn seekability(&self) -> Seekability {
        if self.seekable {
            Seekability::Seekable
        } else {
            Seekability::NotSeekable {
                reason: NotSeekableReason::Unknown,
            }
        }
    }

    fn validators(&self) -> SourceValidators {
        SourceValidators::default()
    }

    fn content_length(&self) -> Option<u64> {
        Some(self.bytes.len() as u64)
    }

    fn fingerprint(&self) -> SourceFingerprint {
        SourceFingerprint::new("ac3-elementary-memory")
    }
}

pub fn seekable_input(bytes: &[u8]) -> DemuxInput {
    DemuxInput::byte_source(Box::new(MemoryByteSource::seekable(bytes)))
}

pub fn stream_input(bytes: &[u8]) -> DemuxInput {
    DemuxInput::byte_stream(Box::new(Cursor::new(bytes.to_vec())))
}

/// Открывает demuxer напрямую с пределами по умолчанию.
pub fn open_direct(input: DemuxInput) -> Ac3ElementaryDemuxer {
    Ac3ElementaryDemuxer::open(
        input,
        CancellationToken::never_cancelled(),
        Ac3ElementaryDemuxOptions::default(),
    )
    .expect("open AC-3 elementary demuxer")
}

/// Registry только с этим factory: проверяет probe → open через нейтральный путь.
pub fn open_through_registry(
    input: DemuxInput,
    extension: Option<&str>,
) -> Result<Box<dyn Demuxer + Send>, demux_api::DemuxOpenError> {
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(
            Ac3ElementaryDemuxFactory::new(Ac3ElementaryDemuxOptions::default())
                .expect("factory descriptor"),
        ))
        .expect("register factory");
    let hints = extension.map_or_else(DemuxHints::none, |extension| {
        DemuxHints::none().with_extension(DemuxSourceExtension::new(extension).expect("ext"))
    });
    registry.open(
        input,
        hints,
        DemuxSniffBudget::new(
            NonZeroUsize::new(256 * 1024).expect("bytes"),
            NonZeroUsize::new(8).expect("segments"),
            Duration::from_secs(2),
        )
        .expect("budget"),
        CancellationToken::never_cancelled(),
    )
}

/// Все пакеты до EOF; повторный вызов после EOF тоже обязан вернуть EOF.
pub fn drain_packets(demuxer: &mut dyn Demuxer) -> Vec<Packet> {
    let mut packets = Vec::new();
    loop {
        match demuxer.next_event().expect("demux event") {
            DemuxReadEvent::Packet(packet) => packets.push(packet),
            DemuxReadEvent::EndOfStream => break,
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert!(matches!(
        demuxer.next_event().expect("event after EOF"),
        DemuxReadEvent::EndOfStream
    ));
    packets
}

/// Следующий пакет (паника на EOF и других событиях).
pub fn next_packet(demuxer: &mut dyn Demuxer) -> Packet {
    match demuxer.next_event().expect("demux event") {
        DemuxReadEvent::Packet(packet) => packet,
        other => panic!("expected packet, got {other:?}"),
    }
}
