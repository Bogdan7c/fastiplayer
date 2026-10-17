//! Каталог: допуск по времени между видео и версиями звука и изоляция несовместимых версий.

use super::*;

/// Видео + две версии звука в одной group (`English` — default, `Commentary` — sibling).
const TWO_VERSION_MASTER: &str = "#EXTM3U\n\
     #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"English\",LANGUAGE=\"en\",CHANNELS=\"2\",URI=\"en.m3u8\"\n\
     #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"Commentary\",LANGUAGE=\"en\",CHANNELS=\"2\",URI=\"commentary.m3u8\"\n\
     #EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.640028,mp4a.40.2\",RESOLUTION=1280x720,AUDIO=\"a\"\n\
     video.m3u8\n";

fn build_two_versions(
    english: HlsCatalogChildProof,
    commentary: HlsCatalogChildProof,
    audio_policy: HlsProviderDefaultAudioPolicy,
) -> Result<HlsCatalogSnapshot, HlsCatalogBuildError> {
    let master = master(TWO_VERSION_MASTER);
    let intent = separate_intent(1280, 720, "English");
    let mut proofs = ProofQueue::new([video_proof(), english, commentary]);
    build_hls_catalog(
        HlsCatalogBuildRequest {
            master: &master,
            catalog_identity: catalog_identity(1, 1),
            provider_default: &intent,
            provider_default_variant_index: None,
            policy: policy().with_provider_default_audio(audio_policy),
        },
        &mut proofs,
    )
}

fn audio_proof_with_discontinuity_at(start_ms: u64) -> HlsCatalogChildProof {
    HlsCatalogChildProof {
        container: HlsRequiredContainer::FragmentedMp4,
        tracks: HlsCatalogTrackProof::AudioOnly(audio()),
        alignment: HlsCatalogAlignmentProof::from_timeline(HlsCatalogTimeline {
            total_duration: Duration::from_millis(REFERENCE_TOTAL_MS),
            target_segment_duration: TEST_TARGET_DURATION,
            discontinuity_starts: vec![Duration::from_millis(start_ms)],
        }),
    }
}

#[test]
fn small_duration_differences_keep_both_versions_selectable() {
    // 10,032 с — нарезка кадрами E-AC-3; 11,9 с — версия без последнего неполного сегмента
    // у видео (меньше одной target duration = 2 с).
    let snapshot = build_two_versions(
        audio_proof_with_total_ms(REFERENCE_TOTAL_MS + 32),
        audio_proof_with_total_ms(REFERENCE_TOTAL_MS + 1_900),
        HlsProviderDefaultAudioPolicy::RequireDeclared,
    )
    .expect("unaligned but compatible versions open");
    assert_eq!(
        snapshot.catalog().required_audio_variants().unwrap().len(),
        2
    );
    assert_eq!(
        snapshot
            .catalog()
            .compatibility()
            .expect("topology relation")
            .logical_edge_count(),
        2,
        "the video pairs with both versions"
    );
    assert!(snapshot.sibling_rejections().is_empty());
}

#[test]
fn sibling_with_other_discontinuities_is_dropped_and_video_rows_are_kept() {
    let snapshot = build_two_versions(
        audio_proof(),
        audio_proof_with_discontinuity_at(5_000),
        HlsProviderDefaultAudioPolicy::RequireDeclared,
    )
    .expect("a broken sibling version must not fail the open");
    assert_eq!(
        snapshot.catalog().required_audio_variants().unwrap().len(),
        1
    );
    assert_eq!(
        snapshot.catalog().required_video_variants().unwrap().len(),
        1,
        "audio isolation never removes video rows"
    );
    let [rejection] = snapshot.sibling_rejections() else {
        panic!("the dropped version is reported once");
    };
    assert_eq!(
        rejection.reason(),
        HlsCatalogSiblingRejectionReason::AudioTimelineDiscontinuityMismatch
    );
}

#[test]
fn default_version_conflict_is_fatal_and_names_the_duration_under_every_policy() {
    for audio_policy in [
        HlsProviderDefaultAudioPolicy::RequireDeclared,
        // Даже native-политика, разрешающая опустить неподдерживаемый кодек, не должна
        // молча играть без звука из-за конфликта самого источника.
        HlsProviderDefaultAudioPolicy::AllowUnsupportedOmission,
    ] {
        let error = build_two_versions(
            audio_proof_with_total_ms(60_000),
            audio_proof(),
            audio_policy,
        )
        .expect_err("default pair conflict must fail");
        assert!(matches!(
            error,
            HlsCatalogBuildError::ProviderDefaultRejected {
                reason: HlsCatalogSiblingRejectionReason::AudioTimelineDurationMismatch
            }
        ));
        assert!(
            error
                .to_string()
                .contains("audio version duration differs from the video"),
            "message names the conflict: {error}"
        );
    }
}
