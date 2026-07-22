use std::fs::{self, File};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use demux_api::{
    DemuxContainerId, DemuxFactory, DemuxHintRelationship, DemuxHints, DemuxInput,
    DemuxInputCapability, DemuxProbeDecision, DemuxProbeRejection, DemuxProbeRequest,
    DemuxRegistry, DemuxSniffBudget, DemuxSourceExtension,
};
use media_core::TrackKind;
use source_core::{CancellationToken, LocalFileSource};

use super::{ContainerDetection, SymphoniaDemuxFactory, detect_container};
use crate::{DemuxerOptions, probe_open_local_media_file};

mod fragmented_isomp4;
mod matroska;
mod ordered_segments;

/// Counter гарантирует unique temp path даже при parallel unit tests одного process-а.
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Drop guard удаляет только exact test-owned path.
struct TemporaryMediaFile {
    path: PathBuf,
}

impl TemporaryMediaFile {
    /// Записывает hermetic bytes в OS temp directory.
    fn new(extension: &str, bytes: &[u8]) -> Self {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rustiplayer-demux-factory-{}-{sequence}.{extension}",
            std::process::id()
        ));
        fs::write(&path, bytes).expect("write hermetic media fixture");
        Self { path }
    }
}

impl Drop for TemporaryMediaFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Генерирует минимальный mono PCM WAV без checked-in media assets.
fn generated_pcm_wav() -> Vec<u8> {
    let sample_data = [0_u8; 32];
    let riff_size = 36_u32 + sample_data.len() as u32;
    let mut wav = Vec::with_capacity(44 + sample_data.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&8_000_u32.to_le_bytes());
    wav.extend_from_slice(&16_000_u32.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(sample_data.len() as u32).to_le_bytes());
    wav.extend_from_slice(&sample_data);
    wav
}

/// Factory capabilities добавляют ordered input только доказанным fragmented rows.
#[test]
fn descriptor_declares_ordered_segments_only_for_proven_fragmented_containers() {
    let factory = SymphoniaDemuxFactory::new(DemuxerOptions::default()).expect("factory");
    let descriptor = factory.descriptor();
    for registration in &descriptor.containers {
        let capabilities = registration.input_capabilities();
        assert!(
            capabilities.contains(DemuxInputCapability::SeekableBytes),
            "{} должен сохранить seekable input",
            registration.container
        );
        assert!(
            capabilities.contains(DemuxInputCapability::StreamingBytes),
            "{} должен сохранить streaming input",
            registration.container
        );
        assert_eq!(
            capabilities.contains(DemuxInputCapability::OrderedSegments),
            matches!(
                registration.container.as_str(),
                "iso-bmff" | "matroska" | "webm"
            ),
            "ordered input разрешён только ISO BMFF и Matroska/WebM"
        );
    }
    assert!(!descriptor.fixture_ids.is_empty());
}

/// Content signature имеет приоритет и сохраняет explicit hint disagreement.
#[test]
fn wave_signature_overrides_disagreeing_mp4_hint() {
    let factory = SymphoniaDemuxFactory::new(DemuxerOptions::default()).expect("factory");
    let hints = DemuxHints::none()
        .with_extension(DemuxSourceExtension::new("mp4").expect("extension"))
        .with_container(DemuxContainerId::new("iso-bmff").expect("container"));
    let decision = factory.probe(DemuxProbeRequest {
        hints: &hints,
        sniffed_bytes: &generated_pcm_wav(),
        input_capability: DemuxInputCapability::SeekableBytes,
        cancellation: &CancellationToken::never_cancelled(),
    });
    let DemuxProbeDecision::Match(matched) = decision else {
        panic!("expected Wave content match");
    };
    assert_eq!(matched.container.as_str(), "wave");
    assert_eq!(matched.hint_relationship, DemuxHintRelationship::Disagrees);
}

/// Prefix known signature-а даёт typed truncation, random bytes — no-match.
#[test]
fn truncated_and_no_match_are_distinct() {
    assert!(matches!(
        detect_container(b"RI", &DemuxHints::none()),
        ContainerDetection::Truncated { required_bytes: 12 }
    ));
    assert!(matches!(
        detect_container(b"not-media", &DemuxHints::none()),
        ContainerDetection::NoMatch
    ));
    assert!(matches!(
        detect_container(b"\0\0\0\x18fty", &DemuxHints::none()),
        ContainerDetection::Truncated { required_bytes: 8 }
    ));

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let factory = SymphoniaDemuxFactory::new(DemuxerOptions::default()).expect("factory");
    assert!(matches!(
        factory.probe(DemuxProbeRequest {
            hints: &DemuxHints::none(),
            sniffed_bytes: b"\0\0\0\x18ftypisom",
            input_capability: DemuxInputCapability::SeekableBytes,
            cancellation: &cancellation,
        }),
        DemuxProbeDecision::Rejected(DemuxProbeRejection::Cancelled)
    ));
}

/// Новый registry open публикует тот же static topology/duration, что existing local probe.
#[test]
fn registry_open_preserves_local_probe_parity() {
    let fixture = TemporaryMediaFile::new("wav", &generated_pcm_wav());
    let cancellation = CancellationToken::never_cancelled();
    let mut file = File::open(&fixture.path).expect("open fixture for local probe");
    let local_snapshot = probe_open_local_media_file(&mut file, Some("wav"), &cancellation)
        .expect("existing local probe");

    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(
            SymphoniaDemuxFactory::new(DemuxerOptions::default()).expect("factory"),
        ))
        .expect("register Symphonia");
    let source = LocalFileSource::open(&fixture.path).expect("local byte source");
    let sniff_budget = DemuxSniffBudget::new(
        NonZeroUsize::new(64).expect("non-zero bytes"),
        NonZeroUsize::new(1).expect("non-zero segments"),
        Duration::from_secs(1),
    )
    .expect("sniff budget");
    let demuxer = registry
        .open(
            DemuxInput::byte_source(Box::new(source)),
            DemuxHints::none().with_extension(DemuxSourceExtension::new("wav").expect("extension")),
            sniff_budget,
            cancellation,
        )
        .expect("registry open");

    let audio_tracks = demuxer
        .tracks()
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .count();
    let video_tracks = demuxer
        .tracks()
        .iter()
        .filter(|track| track.kind == TrackKind::Video)
        .count();
    assert_eq!(audio_tracks, local_snapshot.topology().audio_track_count());
    assert_eq!(video_tracks, local_snapshot.topology().video_track_count());
    assert_eq!(
        demuxer.duration(),
        local_snapshot
            .duration()
            .map(media_core::MediaDuration::as_duration)
    );
}
