//! S20/S21C capability intersection для exact S28C audio families.

use audio_core::{AudioDecodeCapabilitySnapshot, AudioDecodeCodecFamily};
use codec_core::VideoCodec;
use demux_api::{DemuxInputCapabilities, DemuxInputCapability};
use web_media_core::{
    AudioComponentDescriptor, AudioTrackDescriptor, ContainerFamily, NormalizedCodec,
    PreferredHeightPolicy, RawCodecIdentity, StreamLayout, TransportFamily,
};

use super::support::{
    candidate_descriptor, candidate_snapshot, container, empty_video_capabilities, exact_request,
    selection_policy, transport,
};
use crate::{
    CandidateCapabilityRejection, CandidateQualityScore, CandidateRejectionReason,
    CandidateRuntimeRequirements, DemuxCapabilityRegistration, DemuxCapabilitySnapshot,
    HdrSelectionPolicy, PlanningCandidate, PlaybackCapabilitySnapshot, PlaybackComponent,
    PlaybackPlanningError, TransportCapabilityRegistration, TransportCapabilitySnapshot,
    plan_playback,
};

/// Один approved S28C row связывает container identity с exact S20 family.
struct AudioPlanningRow {
    /// Raw yt-dlp-compatible container identity.
    container_raw: &'static str,
    /// Нейтральная normalized container family.
    container_family: ContainerFamily,
    /// Raw codec identity из descriptor-а.
    codec_raw: &'static str,
    /// Exact S20 family query.
    codec_family: AudioDecodeCodecFamily,
}

/// Возвращает representative legal rows без произвольного Cartesian product-а.
fn audio_rows() -> [AudioPlanningRow; 8] {
    [
        AudioPlanningRow {
            container_raw: "ogg",
            container_family: ContainerFamily::Ogg,
            codec_raw: "opus",
            codec_family: AudioDecodeCodecFamily::Opus,
        },
        AudioPlanningRow {
            container_raw: "caf",
            container_family: ContainerFamily::Caf,
            codec_raw: "pcm_s16le",
            codec_family: AudioDecodeCodecFamily::Pcm,
        },
        AudioPlanningRow {
            container_raw: "wav",
            container_family: ContainerFamily::Wav,
            codec_raw: "pcm_s16le",
            codec_family: AudioDecodeCodecFamily::Pcm,
        },
        AudioPlanningRow {
            container_raw: "aiff",
            container_family: ContainerFamily::Aiff,
            codec_raw: "pcm_s16be",
            codec_family: AudioDecodeCodecFamily::Pcm,
        },
        AudioPlanningRow {
            container_raw: "flac",
            container_family: ContainerFamily::Flac,
            codec_raw: "flac",
            codec_family: AudioDecodeCodecFamily::Flac,
        },
        AudioPlanningRow {
            container_raw: "mp1",
            container_family: ContainerFamily::MpegAudio,
            codec_raw: "mp1",
            codec_family: AudioDecodeCodecFamily::Mp1,
        },
        AudioPlanningRow {
            container_raw: "mp2",
            container_family: ContainerFamily::MpegAudio,
            codec_raw: "mp2",
            codec_family: AudioDecodeCodecFamily::Mp2,
        },
        AudioPlanningRow {
            container_raw: "mp3",
            container_family: ContainerFamily::MpegAudio,
            codec_raw: "mp3",
            codec_family: AudioDecodeCodecFamily::Mp3,
        },
    ]
}

/// Строит audio-only candidate с exact descriptor/runtime family parity.
fn candidate(row: &AudioPlanningRow) -> PlanningCandidate {
    let audio_track = AudioTrackDescriptor::new(
        NormalizedCodec::parse(
            RawCodecIdentity::new(row.codec_raw).expect("S28C raw codec identity valid"),
        ),
        None,
        None,
        None,
        None,
    );
    let component = AudioComponentDescriptor::new(
        transport("https"),
        container(row.container_raw),
        audio_track,
    );
    PlanningCandidate::new(
        candidate_descriptor(
            row.codec_raw,
            &format!("{}-semantic", row.codec_raw),
            StreamLayout::AudioOnly(component),
        ),
        CandidateRuntimeRequirements::AudioOnly {
            audio: row.codec_family,
        },
        CandidateQualityScore::new(1),
    )
    .expect("S28C candidate descriptor/runtime family agreed")
}

/// Строит exact progressive transport/demux capability для одной container family.
fn resource_capabilities(
    container_family: ContainerFamily,
) -> (TransportCapabilitySnapshot, DemuxCapabilitySnapshot) {
    let byte_inputs = DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes)
        .with(DemuxInputCapability::StreamingBytes);
    let transport = TransportCapabilitySnapshot::new(vec![
        TransportCapabilityRegistration::new(
            TransportFamily::ProgressiveHttp(web_media_core::HttpScheme::Https),
            byte_inputs,
        )
        .expect("S28C transport registration valid"),
    ]);
    let demux = DemuxCapabilitySnapshot::new(vec![
        DemuxCapabilityRegistration::new(container_family, byte_inputs)
            .expect("S28C demux registration valid"),
    ]);
    (transport, demux)
}

/// Policy не влияет на audio-only selection, но сохраняет explicit stable ordering contract.
fn policy(container_family: ContainerFamily) -> crate::PlaybackSelectionPolicy {
    selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        vec![VideoCodec::Vp9],
        vec![container_family],
    )
}

/// Exact available family проходит S20/S21C без создания decoder-а или I/O.
#[test]
fn current_audio_container_rows_require_exact_available_s20_family() {
    for row in audio_rows() {
        let candidate = candidate(&row);
        let request = exact_request(&candidate);
        let snapshot = candidate_snapshot(vec![candidate]);
        let (transport, demux) = resource_capabilities(row.container_family);
        let video = empty_video_capabilities();
        let audio = AudioDecodeCapabilitySnapshot::empty().with_available_family(row.codec_family);
        let capabilities = PlaybackCapabilitySnapshot::new(&transport, &demux, &video, audio);
        let outcome = plan_playback(
            &snapshot,
            capabilities,
            &request,
            &policy(row.container_family),
        )
        .expect("exact S28C family must pass capability intersection");
        assert_eq!(
            outcome.selected().layout(),
            web_media_core::StreamLayoutKind::AudioOnly
        );
    }
}

/// MP1/MP2/MP3 и соседние rows дают отдельный typed rejection без wildcard family.
#[test]
fn unavailable_s28c_audio_family_is_rejected_before_io() {
    for row in audio_rows() {
        let candidate = candidate(&row);
        let request = exact_request(&candidate);
        let snapshot = candidate_snapshot(vec![candidate]);
        let (transport, demux) = resource_capabilities(row.container_family);
        let video = empty_video_capabilities();
        let capabilities = PlaybackCapabilitySnapshot::new(
            &transport,
            &demux,
            &video,
            AudioDecodeCapabilitySnapshot::empty(),
        );
        let PlaybackPlanningError::ExactCandidateNotPlayable(rejection) = plan_playback(
            &snapshot,
            capabilities,
            &request,
            &policy(row.container_family),
        )
        .expect_err("missing exact S20 family should reject candidate") else {
            panic!("It is expected to be exact typed audio rejection.");
        };
        assert!(rejection.reasons().iter().any(|reason| matches!(
            reason,
            CandidateRejectionReason::Capability(
                CandidateCapabilityRejection::AudioUnavailable {
                    component: PlaybackComponent::Audio,
                    family,
                }
            ) if *family == row.codec_family
        )));
    }
}

/// AC-3/E-AC-3 rows в ISO-BMFF: две разные exact family без взаимной подмены.
fn dolby_rows() -> [AudioPlanningRow; 2] {
    [
        AudioPlanningRow {
            container_raw: "mp4",
            container_family: ContainerFamily::IsoBmff,
            codec_raw: "ac-3",
            codec_family: AudioDecodeCodecFamily::Ac3,
        },
        AudioPlanningRow {
            container_raw: "mp4",
            container_family: ContainerFamily::IsoBmff,
            codec_raw: "ec-3",
            codec_family: AudioDecodeCodecFamily::Eac3,
        },
    ]
}

/// Planner принимает AC-3/E-AC-3 только при доступной exact family; соседняя family
/// (AC-3 вместо E-AC-3 и наоборот) не считается доказательством capability.
#[test]
fn dolby_audio_rows_intersect_exact_snapshot_family() {
    for row in dolby_rows() {
        let other_family = match row.codec_family {
            AudioDecodeCodecFamily::Ac3 => AudioDecodeCodecFamily::Eac3,
            _ => AudioDecodeCodecFamily::Ac3,
        };
        let (transport, demux) = resource_capabilities(row.container_family);
        let video = empty_video_capabilities();

        let accepted_candidate = candidate(&row);
        let accepted_request = exact_request(&accepted_candidate);
        let accepted_snapshot = candidate_snapshot(vec![accepted_candidate]);
        let available = AudioDecodeCapabilitySnapshot::empty()
            .with_available_family(row.codec_family)
            .with_available_family(AudioDecodeCodecFamily::Aac);
        let outcome = plan_playback(
            &accepted_snapshot,
            PlaybackCapabilitySnapshot::new(&transport, &demux, &video, available),
            &accepted_request,
            &policy(row.container_family),
        )
        .expect("available exact Dolby family must pass capability intersection");
        assert_eq!(
            outcome.selected().layout(),
            web_media_core::StreamLayoutKind::AudioOnly
        );

        let rejected_candidate = candidate(&row);
        let rejected_request = exact_request(&rejected_candidate);
        let rejected_snapshot = candidate_snapshot(vec![rejected_candidate]);
        let neighbour_only = AudioDecodeCapabilitySnapshot::empty()
            .with_available_family(other_family)
            .with_available_family(AudioDecodeCodecFamily::Aac);
        let PlaybackPlanningError::ExactCandidateNotPlayable(rejection) = plan_playback(
            &rejected_snapshot,
            PlaybackCapabilitySnapshot::new(&transport, &demux, &video, neighbour_only),
            &rejected_request,
            &policy(row.container_family),
        )
        .expect_err("neighbour Dolby family must not prove capability") else {
            panic!("It is expected to be exact typed audio rejection.");
        };
        assert!(rejection.reasons().iter().any(|reason| matches!(
            reason,
            CandidateRejectionReason::Capability(
                CandidateCapabilityRejection::AudioUnavailable {
                    component: PlaybackComponent::Audio,
                    family,
                }
            ) if *family == row.codec_family
        )));
    }
}

/// Descriptor `ec-3` не может нести runtime requirement AC-3: admission ловит подмену.
#[test]
fn dolby_descriptor_and_runtime_family_must_agree() {
    let [ac3_row, _] = dolby_rows();
    let audio_track = AudioTrackDescriptor::new(
        NormalizedCodec::parse(RawCodecIdentity::new("ec-3").expect("codec identity valid")),
        None,
        None,
        None,
        None,
    );
    let component =
        AudioComponentDescriptor::new(transport("https"), container("mp4"), audio_track);
    let error = PlanningCandidate::new(
        candidate_descriptor("ec-3", "ec-3-semantic", StreamLayout::AudioOnly(component)),
        CandidateRuntimeRequirements::AudioOnly {
            audio: ac3_row.codec_family,
        },
        CandidateQualityScore::new(1),
    )
    .expect_err("E-AC-3 descriptor with AC-3 runtime family must be rejected");
    assert!(matches!(
        error,
        crate::PlanningCandidateBuildError::AudioCodecMismatch { .. }
    ));
}
