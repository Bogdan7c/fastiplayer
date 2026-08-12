//! P3B source lifecycle, security и reconstruction cases.

use super::*;

#[test]
fn construction_and_initialization_are_lazy_and_cancellation_does_not_advance() {
    let origin = FixtureOrigin::start();
    let prepared = prepare(&origin);
    let selected = selection(&prepared, 401_000);
    let sources = prepared
        .into_selected_fragment_sources(selected, fragment_policy())
        .expect("selected sources");
    assert_eq!(
        origin.request_count(),
        1,
        "construction must not fetch media"
    );
    let mut video = sources.into_source_parts().video;
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        video.next_segment(&cancelled),
        Err(OrderedSegmentReadError::Cancelled)
    ));
    assert_eq!(origin.request_count(), 1, "cancelled init must not fetch");
    let initialization = video
        .next_segment(&CancellationToken::new())
        .expect("initialization read")
        .expect("initialization segment");
    assert_eq!(initialization.sequence.get(), 0);
    assert_eq!(initialization.kind, OrderedSegmentKind::Initialization);
    assert_eq!(
        initialization.discontinuity,
        demux_api::OrderedSegmentDiscontinuity::Continuous
    );
    assert_eq!(origin.request_count(), 1, "initialization is retained");
    let cancelled_media = CancellationToken::new();
    cancelled_media.cancel();
    assert!(matches!(
        video.next_segment(&cancelled_media),
        Err(OrderedSegmentReadError::Cancelled)
    ));
    assert_eq!(
        origin.request_count(),
        1,
        "cancelled media pull must not fetch or advance"
    );
    let media = video
        .next_segment(&CancellationToken::new())
        .expect("first low-video read")
        .expect("first low-video segment");
    assert_eq!(media.sequence.get(), 1);
    assert_eq!(media.kind, OrderedSegmentKind::Media);
    assert_eq!(
        media.discontinuity,
        demux_api::OrderedSegmentDiscontinuity::Continuous
    );
    assert_eq!(
        origin.request_targets(),
        [
            "/media/tears-of-steel.ismc",
            "/media/QualityLevels(401000)/Fragments(video_eng=0)",
        ]
    );
}

#[test]
fn stale_selection_is_rejected_before_source_construction() {
    let origin = FixtureOrigin::start();
    let stale_catalog = prepare_with_generation(&origin, 43);
    let stale_selection = selection(&stale_catalog, 401_000);
    let current_catalog = prepare_with_generation(&origin, 44);
    let requests_before_construction = origin.request_count();

    let error = current_catalog
        .into_selected_fragment_sources(stale_selection, fragment_policy())
        .expect_err("stale catalog generation must fail exact selection");

    assert!(matches!(
        error,
        SmoothFragmentSourceBuildError::Selection(_)
    ));
    assert_eq!(origin.request_count(), requests_before_construction);
}

#[test]
fn wrong_selection_layout_is_rejected_before_source_construction() {
    let origin = FixtureOrigin::start();
    let prepared = prepare(&origin);
    let video_variant = prepared
        .catalog()
        .required_video_variants()
        .expect("video axis")
        .iter()
        .find(|variant| {
            variant
                .track()
                .bitrate()
                .is_some_and(|bitrate| bitrate.bits_per_second() == 401_000)
        })
        .expect("401 kbps video")
        .clone();
    let wrong_layout_catalog = ComponentVariantCatalog::new(
        prepared.catalog().identity().clone(),
        ComponentVariantCatalogLimit::new(1).expect("single variant limit"),
        ComponentVariantCatalogEntries::VideoOnly {
            video: vec![video_variant.clone()],
        },
    )
    .expect("video-only test catalog");
    let wrong_layout_selection = wrong_layout_catalog
        .select_exact(ComponentVariantSelectionRequest::VideoOnly {
            video: video_variant.exact_identity().clone(),
        })
        .expect("video-only selection");
    let requests_before_construction = origin.request_count();

    let error = prepared
        .into_selected_fragment_sources(wrong_layout_selection, fragment_policy())
        .expect_err("VideoOnly selection cannot build VideoAndAudio sources");

    assert!(matches!(
        error,
        SmoothFragmentSourceBuildError::Selection(_)
    ));
    assert_eq!(origin.request_count(), requests_before_construction);
}

#[test]
fn terminal_fetch_failure_is_latched_and_redacted_without_refetch() {
    let origin = FixtureOrigin::start();
    let prepared = prepare(&origin);
    let selected = selection(&prepared, 751_000);
    let mut video = prepared
        .into_selected_fragment_sources(selected, fragment_policy())
        .expect("selected sources")
        .into_source_parts()
        .video;
    let cancellation = CancellationToken::new();
    video
        .next_segment(&cancellation)
        .expect("initialization read")
        .expect("initialization");
    let first_error = video
        .next_segment(&cancellation)
        .expect_err("missing corpus fragment must fail");
    let requests_after_failure = origin.request_count();
    let second_error = video
        .next_segment(&cancellation)
        .expect_err("terminal failure must remain latched");

    assert_eq!(origin.request_count(), requests_after_failure);
    assert_eq!(format!("{first_error:?}"), format!("{second_error:?}"));
    let diagnostics = format!("{first_error:?}");
    assert!(!diagnostics.contains("QualityLevels"));
    assert!(!diagnostics.contains("751000"));
    assert!(!diagnostics.contains("127.0.0.1"));
}

#[test]
fn reconstruction_limit_failure_is_latched_after_one_fetch() {
    let origin = FixtureOrigin::start();
    let prepared = prepare(&origin);
    let selected = selection(&prepared, 1_501_000);
    let mut video = prepared
        .into_selected_fragment_sources(
            selected,
            fragment_policy_with_max_input(VIDEO_HIGH_FIRST.len() - 1),
        )
        .expect("selected sources")
        .into_source_parts()
        .video;
    let cancellation = CancellationToken::new();
    video
        .next_segment(&cancellation)
        .expect("initialization read")
        .expect("initialization");
    assert!(matches!(
        video.next_segment(&cancellation),
        Err(OrderedSegmentReadError::Failed { .. })
    ));
    let requests_after_failure = origin.request_count();
    assert!(matches!(
        video.next_segment(&cancellation),
        Err(OrderedSegmentReadError::Failed { .. })
    ));
    assert_eq!(origin.request_count(), requests_after_failure);
}

#[test]
fn retained_transport_body_limit_rejects_before_reconstruction_and_latches() {
    let origin = FixtureOrigin::start();
    let source_config =
        SourceRuntimeConfig::from_network_config(&NetworkConfig::default()).expect("source config");
    let prepared = crate::prepare::prepare_smooth_vod_all_for_test(SmoothPrepareRequest::new(
        transport_request(origin.target()),
        &source_config,
        ComponentVariantCatalogGeneration::new(44),
        PreferredHeightPolicy::NoPreference,
        preparation_policy_with_segment_limit(VIDEO_LOW_FIRST.len() - 1),
    ))
    .expect("canonical preparation");
    let selected = selection(&prepared, 401_000);
    let mut video = prepared
        .into_selected_fragment_sources(selected, fragment_policy())
        .expect("selected sources")
        .into_source_parts()
        .video;
    let cancellation = CancellationToken::new();
    video
        .next_segment(&cancellation)
        .expect("initialization read")
        .expect("initialization");
    assert!(matches!(
        video.next_segment(&cancellation),
        Err(OrderedSegmentReadError::Failed { .. })
    ));
    let requests_after_failure = origin.request_count();
    assert!(matches!(
        video.next_segment(&cancellation),
        Err(OrderedSegmentReadError::Failed { .. })
    ));
    assert_eq!(origin.request_count(), requests_after_failure);
}

#[test]
fn reconstruction_write_limit_rejects_and_latches_after_one_fetch() {
    let origin = FixtureOrigin::start();
    let prepared = prepare(&origin);
    let selected = selection(&prepared, 401_000);
    let mut video = prepared
        .into_selected_fragment_sources(selected, fragment_policy_with_limits(128 * 1_024, 16))
        .expect("selected sources")
        .into_source_parts()
        .video;
    let cancellation = CancellationToken::new();
    video
        .next_segment(&cancellation)
        .expect("initialization read")
        .expect("initialization");
    assert!(matches!(
        video.next_segment(&cancellation),
        Err(OrderedSegmentReadError::Failed { .. })
    ));
    let requests_after_failure = origin.request_count();
    assert!(matches!(
        video.next_segment(&cancellation),
        Err(OrderedSegmentReadError::Failed { .. })
    ));
    assert_eq!(origin.request_count(), requests_after_failure);
}

#[test]
fn end_of_stream_is_repeatable_and_performs_no_http() {
    let origin = FixtureOrigin::start();
    let prepared = prepare(&origin);
    let selected = selection(&prepared, 401_000);
    let mut video = prepared
        .into_selected_fragment_sources(selected, fragment_policy())
        .expect("selected sources")
        .into_source_parts()
        .video;
    video.end_after_initialization_for_test();
    let cancellation = CancellationToken::new();
    video
        .next_segment(&cancellation)
        .expect("initialization read")
        .expect("initialization");
    let requests_before_eos = origin.request_count();

    assert!(
        video
            .next_segment(&cancellation)
            .expect("first EOS")
            .is_none()
    );
    assert!(
        video
            .next_segment(&cancellation)
            .expect("second EOS")
            .is_none()
    );
    assert_eq!(origin.request_count(), requests_before_eos);
}

#[test]
fn fragment_paths_use_effective_manifest_base_and_cross_origin_secret_stays_stripped() {
    let effective_origin = FixtureOrigin::start();
    let redirect_listener = TcpListener::bind("127.0.0.1:0").expect("redirect listener");
    let redirect_address = redirect_listener.local_addr().expect("redirect address");
    let initial_target =
        HttpRequestTarget::parse_exact(format!("http://{redirect_address}/entry.ismc"))
            .expect("initial manifest target");
    let redirect_worker = serve_redirect_once(
        redirect_listener,
        effective_origin.exact_target().to_owned(),
    );
    let source_config =
        SourceRuntimeConfig::from_network_config(&NetworkConfig::default()).expect("source config");
    let prepared = crate::prepare::prepare_smooth_vod_all_for_test(SmoothPrepareRequest::new(
        transport_request_with_security(
            &initial_target,
            RedirectPolicy::cross_origin_without_secrets(
                RedirectHopLimit::new(2).expect("redirect budget"),
            ),
            true,
        ),
        &source_config,
        ComponentVariantCatalogGeneration::new(44),
        PreferredHeightPolicy::NoPreference,
        preparation_policy(),
    ))
    .expect("redirected canonical preparation");
    let initial_request = redirect_worker.join().expect("redirect worker");
    let selected = selection(&prepared, 401_000);
    let mut video = prepared
        .into_selected_fragment_sources(selected, fragment_policy())
        .expect("selected sources")
        .into_source_parts()
        .video;
    let cancellation = CancellationToken::new();
    video
        .next_segment(&cancellation)
        .expect("initialization read")
        .expect("initialization");
    video
        .next_segment(&cancellation)
        .expect("media read")
        .expect("media");

    assert!(
        initial_request
            .to_ascii_lowercase()
            .contains("authorization:")
    );
    assert!(
        initial_request
            .to_ascii_lowercase()
            .contains("p3b-cookie-secret")
    );
    let effective_requests = effective_origin
        .requests
        .lock()
        .expect("request journal")
        .clone();
    assert!(effective_requests.iter().all(|request| {
        let normalized = request.to_ascii_lowercase();
        !normalized.contains("authorization:")
            && !normalized.contains("cookie:")
            && !normalized.contains("p3b-cookie-secret")
    }));
    assert_eq!(
        effective_origin.request_targets(),
        [
            "/media/tears-of-steel.ismc",
            "/media/QualityLevels(401000)/Fragments(video_eng=0)",
        ]
    );
}

#[test]
fn same_scope_manifest_and_fragment_keep_required_scoped_secrets() {
    let origin = FixtureOrigin::start();
    let source_config =
        SourceRuntimeConfig::from_network_config(&NetworkConfig::default()).expect("source config");
    let prepared = crate::prepare::prepare_smooth_vod_all_for_test(SmoothPrepareRequest::new(
        transport_request_with_security(
            origin.target(),
            RedirectPolicy::same_origin(RedirectHopLimit::new(2).expect("redirect budget")),
            true,
        ),
        &source_config,
        ComponentVariantCatalogGeneration::new(44),
        PreferredHeightPolicy::NoPreference,
        preparation_policy(),
    ))
    .expect("same-scope canonical preparation");
    let selected = selection(&prepared, 401_000);
    let mut video = prepared
        .into_selected_fragment_sources(selected, fragment_policy())
        .expect("selected sources")
        .into_source_parts()
        .video;
    let cancellation = CancellationToken::new();
    video
        .next_segment(&cancellation)
        .expect("initialization read")
        .expect("initialization");
    video
        .next_segment(&cancellation)
        .expect("media read")
        .expect("media");

    let requests = origin.requests.lock().expect("request journal").clone();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| {
        let normalized = request.to_ascii_lowercase();
        normalized.contains("authorization:") && normalized.contains("p3b-cookie-secret")
    }));
}

#[test]
fn high_video_preserves_exact_fragment_order_and_continuous_sequences() {
    let origin = FixtureOrigin::start();
    let prepared = prepare(&origin);
    let selected = selection(&prepared, 1_501_000);
    let mut video = prepared
        .into_selected_fragment_sources(selected, fragment_policy())
        .expect("selected sources")
        .into_source_parts()
        .video;
    let cancellation = CancellationToken::new();
    let initialization = video
        .next_segment(&cancellation)
        .expect("initialization read")
        .expect("initialization");
    let first = video
        .next_segment(&cancellation)
        .expect("first media read")
        .expect("first media");
    let second = video
        .next_segment(&cancellation)
        .expect("second media read")
        .expect("second media");
    assert_eq!(
        [
            initialization.sequence.get(),
            first.sequence.get(),
            second.sequence.get(),
        ],
        [0, 1, 2]
    );
    assert_eq!(
        origin.request_targets(),
        [
            "/media/tears-of-steel.ismc",
            "/media/QualityLevels(1501000)/Fragments(video_eng=0)",
            "/media/QualityLevels(1501000)/Fragments(video_eng=40000000)",
        ]
    );
}

#[test]
fn audio_emits_exact_bounded_windows_and_preserves_f2_pending_bytes() {
    let origin = FixtureOrigin::start();
    let prepared = prepare(&origin);
    let selected = selection(&prepared, 401_000);
    let mut audio = prepared
        .into_selected_fragment_sources(selected, fragment_policy())
        .expect("selected sources")
        .into_source_parts()
        .audio;
    let cancellation = CancellationToken::new();
    assert!(matches!(
        audio.next_segment(&cancellation).expect("audio init"),
        PresentationWindowOrderedSegmentReadOutcome::Segment(
            PresentationWindowOrderedSegment::Initialization { .. }
        )
    ));
    let first = audio
        .next_segment(&cancellation)
        .expect("first audio media");
    let second = audio
        .next_segment(&cancellation)
        .expect("second audio media");
    let expected_first = direct_f2_audio_bytes(0, AUDIO_FIRST);
    let expected_second = direct_f2_audio_bytes(1, AUDIO_SECOND);
    assert_audio_window(first, 1, &expected_first, 0, 39_680_000);
    assert_audio_window(second, 2, &expected_second, 39_680_000, 79_573_333);
    assert_eq!(
        origin.request_targets(),
        [
            "/media/tears-of-steel.ismc",
            "/media/QualityLevels(64008)/Fragments(audio_eng=0)",
            "/media/QualityLevels(64008)/Fragments(audio_eng=39680000)",
        ]
    );
}

#[test]
fn audio_subsample_underrun_crosses_fragment_boundary_with_bounded_window() {
    // Canonical second fragment заканчивается на tick позже окна; минус два моделирует live underrun в tick.
    let mut subsample_underrun = AUDIO_SECOND.to_vec();
    adjust_last_trun_duration(&mut subsample_underrun, -2);
    let origin = FixtureOrigin::start_with_fragment(AUDIO_SECOND_PATH, subsample_underrun.clone());
    let prepared = prepare(&origin);
    let selected = selection(&prepared, 401_000);
    let mut audio = prepared
        .into_selected_fragment_sources(selected, fragment_policy())
        .expect("selected sources")
        .into_source_parts()
        .audio;
    let cancellation = CancellationToken::new();

    assert!(matches!(
        audio
            .next_segment(&cancellation)
            .expect("audio initialization"),
        PresentationWindowOrderedSegmentReadOutcome::Segment(
            PresentationWindowOrderedSegment::Initialization { .. }
        )
    ));
    assert!(matches!(
        audio
            .next_segment(&cancellation)
            .expect("first audio fragment"),
        PresentationWindowOrderedSegmentReadOutcome::Segment(
            PresentationWindowOrderedSegment::Media { .. }
        )
    ));
    let second = audio
        .next_segment(&cancellation)
        .expect("subsample underrun must remain playable");
    let expected_second = direct_f2_audio_bytes(1, &subsample_underrun);
    assert_audio_window(second, 2, &expected_second, 39_680_000, 79_573_333);
    assert_eq!(
        origin.request_targets(),
        [
            "/media/tears-of-steel.ismc",
            "/media/QualityLevels(64008)/Fragments(audio_eng=0)",
            "/media/QualityLevels(64008)/Fragments(audio_eng=39680000)",
        ]
    );
}

#[test]
fn audio_full_frame_underrun_still_fails_and_latches() {
    // 209 ticks при 48 kHz и timescale 10 MHz уже выходят за один PCM frame.
    let mut full_frame_underrun = AUDIO_SECOND.to_vec();
    adjust_last_trun_duration(&mut full_frame_underrun, -210);
    let origin = FixtureOrigin::start_with_fragment(AUDIO_SECOND_PATH, full_frame_underrun);
    let prepared = prepare(&origin);
    let selected = selection(&prepared, 401_000);
    let mut audio = prepared
        .into_selected_fragment_sources(selected, fragment_policy())
        .expect("selected sources")
        .into_source_parts()
        .audio;
    let cancellation = CancellationToken::new();

    assert!(matches!(
        audio
            .next_segment(&cancellation)
            .expect("audio initialization"),
        PresentationWindowOrderedSegmentReadOutcome::Segment(
            PresentationWindowOrderedSegment::Initialization { .. }
        )
    ));
    assert!(matches!(
        audio
            .next_segment(&cancellation)
            .expect("first audio fragment"),
        PresentationWindowOrderedSegmentReadOutcome::Segment(
            PresentationWindowOrderedSegment::Media { .. }
        )
    ));
    let first_error = audio
        .next_segment(&cancellation)
        .expect_err("full-frame audio underrun must fail reconstruction");
    let requests_after_failure = origin.request_count();
    let second_error = audio
        .next_segment(&cancellation)
        .expect_err("reconstruction failure must latch");
    assert_eq!(origin.request_count(), requests_after_failure);
    assert_eq!(format!("{first_error:?}"), format!("{second_error:?}"));
    assert!(format!("{first_error:?}").contains("smooth fragment reconstruction failed"));
}

#[test]
fn corpus_derived_exact_audio_emits_unbounded_window() {
    let mut exact_second = AUDIO_SECOND.to_vec();
    adjust_last_trun_duration(&mut exact_second, -1);
    let origin = FixtureOrigin::start_with_fragment(AUDIO_SECOND_PATH, exact_second);
    let prepared = prepare(&origin);
    let selected = selection(&prepared, 401_000);
    let mut audio = prepared
        .into_selected_fragment_sources(selected, fragment_policy())
        .expect("selected sources")
        .into_source_parts()
        .audio;
    let cancellation = CancellationToken::new();
    audio.next_segment(&cancellation).expect("audio init");
    audio
        .next_segment(&cancellation)
        .expect("first bounded audio");
    let second = audio
        .next_segment(&cancellation)
        .expect("exact second audio");
    let PresentationWindowOrderedSegmentReadOutcome::Segment(
        PresentationWindowOrderedSegment::Media {
            sequence,
            discontinuity,
            bytes,
            presentation_window: PacketPresentationWindow::Unbounded,
        },
    ) = second
    else {
        panic!("exact audio must be admitted as unbounded media");
    };
    assert_eq!(sequence.get(), 2);
    assert_eq!(
        discontinuity,
        demux_api::OrderedSegmentDiscontinuity::Continuous
    );
    assert!(!bytes.is_empty());
}

#[test]
fn malformed_start_mismatch_underrun_and_overhang_latch_reconstruction_failure() {
    let mut underrun = VIDEO_HIGH_FIRST.to_vec();
    adjust_last_trun_duration(&mut underrun, -1);
    let mut overhang = VIDEO_HIGH_FIRST.to_vec();
    adjust_last_trun_duration(&mut overhang, 1);
    let mut start_mismatch = VIDEO_HIGH_FIRST.to_vec();
    insert_mismatching_tfdt(&mut start_mismatch);
    for invalid_fragment in [vec![0_u8, 1, 2, 3], start_mismatch, underrun, overhang] {
        let origin = FixtureOrigin::start_with_fragment(VIDEO_HIGH_FIRST_PATH, invalid_fragment);
        let prepared = prepare(&origin);
        let selected = selection(&prepared, 1_501_000);
        let mut video = prepared
            .into_selected_fragment_sources(selected, fragment_policy())
            .expect("selected sources")
            .into_source_parts()
            .video;
        let cancellation = CancellationToken::new();
        video
            .next_segment(&cancellation)
            .expect("initialization read")
            .expect("initialization");
        let first_error = video
            .next_segment(&cancellation)
            .expect_err("invalid fragment must fail reconstruction");
        let requests_after_failure = origin.request_count();
        let second_error = video
            .next_segment(&cancellation)
            .expect_err("reconstruction failure must latch");
        assert_eq!(origin.request_count(), requests_after_failure);
        assert_eq!(format!("{first_error:?}"), format!("{second_error:?}"));
        assert!(format!("{first_error:?}").contains("smooth fragment reconstruction failed"));
    }
}

/// Получает authoritative pending bytes напрямую из F2 для adapter boundary oracle.
fn direct_f2_audio_bytes(fragment_index: usize, input: &[u8]) -> Vec<u8> {
    let manifest = crate::test_support::parse(MANIFEST);
    let selection = manifest
        .streams()
        .iter()
        .enumerate()
        .find_map(|(stream_ordinal, stream)| {
            if stream.kind() != SmoothStreamKind::Audio {
                return None;
            }
            stream.qualities().iter().find_map(|quality| {
                let SmoothQualityLevel::Audio(audio) = quality else {
                    return None;
                };
                (audio.bitrate().get() == 64_008).then(|| {
                    SmoothTrackSelection::new(
                        SmoothStreamOrdinal::new(stream_ordinal),
                        quality.index(),
                    )
                })
            })
        })
        .expect("64 kbps audio selection");
    let not_cancelled = || false;
    let mapped = map_smooth_track(SmoothTrackMappingRequest::new(
        &manifest,
        selection,
        &not_cancelled,
    ))
    .expect("direct F2 mapping");
    let plan = plan_smooth_fragment(SmoothFragmentPlanRequest::new(
        &mapped,
        SmoothFragmentIndex::new(fragment_index),
        &not_cancelled,
    ))
    .expect("direct F2 plan");
    let policy = fragment_policy();
    let reconstructed = reconstruct_smooth_fragment(SmoothFragmentReconstructionRequest::new(
        input,
        &plan,
        &policy.inspection_limits,
        policy.write_limits,
        &not_cancelled,
    ))
    .expect("direct F2 reconstruction");
    let SmoothReconstructedFragment::PendingAudioPresentationWindow(pending) = reconstructed else {
        panic!("canonical audio must retain exact pending proof");
    };
    pending.into_unchanged_media_segment_bytes()
}

/// Проверяет exact F3A metadata и отсутствие byte rewrite для pending audio.
fn assert_audio_window(
    outcome: PresentationWindowOrderedSegmentReadOutcome,
    expected_sequence: u64,
    expected_bytes: &[u8],
    expected_start: i64,
    expected_end_exclusive: i64,
) {
    let PresentationWindowOrderedSegmentReadOutcome::Segment(
        PresentationWindowOrderedSegment::Media {
            sequence,
            bytes,
            presentation_window: PacketPresentationWindow::Bounded(window),
            discontinuity,
            ..
        },
    ) = outcome
    else {
        panic!("expected bounded audio media segment");
    };
    assert_eq!(sequence.get(), expected_sequence);
    assert_eq!(
        discontinuity,
        demux_api::OrderedSegmentDiscontinuity::Continuous
    );
    assert_eq!(bytes.as_ref(), expected_bytes);
    assert_eq!(window.start().track_id.get(), 1);
    assert_eq!(window.end_exclusive().track_id.get(), 1);
    assert_eq!(window.start().units.get(), expected_start);
    assert_eq!(window.end_exclusive().units.get(), expected_end_exclusive);
    assert_eq!(window.start().time_base.numer, 1);
    assert_eq!(window.start().time_base.denom, 10_000_000);
}

/// Меняет последний explicit sample duration, сохраняя structural layout.
fn adjust_last_trun_duration(bytes: &mut [u8], delta: i32) {
    let trun = iso_box_start(bytes, *b"trun");
    let flags = read_u32(bytes, trun + 8) & 0x00ff_ffff;
    assert_ne!(flags & 0x000100, 0, "fixture has sample durations");
    let sample_count = usize::try_from(read_u32(bytes, trun + 12)).expect("sample count");
    let mut rows_start = trun + 16;
    if flags & 0x000001 != 0 {
        rows_start += 4;
    }
    if flags & 0x000004 != 0 {
        rows_start += 4;
    }
    let active_fields: Vec<u32> = [0x000100_u32, 0x000200, 0x000400, 0x000800]
        .into_iter()
        .filter(|field| flags & field != 0)
        .collect();
    let duration_field = active_fields
        .iter()
        .position(|field| *field == 0x000100)
        .expect("duration field");
    let duration_offset =
        rows_start + (sample_count - 1) * active_fields.len() * 4 + duration_field * 4;
    let duration = read_u32(bytes, duration_offset);
    let changed = if delta.is_negative() {
        duration - delta.unsigned_abs()
    } else {
        duration + delta.unsigned_abs()
    };
    bytes[duration_offset..duration_offset + 4].copy_from_slice(&changed.to_be_bytes());
}

/// Вставляет optional `tfdt` v1 и сохраняет structural offsets.
fn insert_mismatching_tfdt(bytes: &mut Vec<u8>) {
    let moof = iso_box_start(bytes, *b"moof");
    let traf = iso_box_start(bytes, *b"traf");
    let traf_end = traf + usize::try_from(read_u32(bytes, traf)).expect("traf size");
    let mut tfdt = Vec::with_capacity(20);
    tfdt.extend_from_slice(&20_u32.to_be_bytes());
    tfdt.extend_from_slice(b"tfdt");
    tfdt.extend_from_slice(&[1, 0, 0, 0]);
    tfdt.extend_from_slice(&1_u64.to_be_bytes());
    bytes.splice(traf_end..traf_end, tfdt);
    for box_offset in [traf, moof] {
        let changed_size = read_u32(bytes, box_offset) + 20;
        bytes[box_offset..box_offset + 4].copy_from_slice(&changed_size.to_be_bytes());
    }
    let changed_moof_end = moof + usize::try_from(read_u32(bytes, moof)).expect("moof size");
    let trun = iso_box_start(bytes, *b"trun");
    let data_offset = u32::try_from(changed_moof_end + 8 - moof).expect("data offset");
    bytes[trun + 16..trun + 20].copy_from_slice(&data_offset.to_be_bytes());
}

/// Находит последний structurally valid ISO box данного type.
fn iso_box_start(bytes: &[u8], box_type: [u8; 4]) -> usize {
    bytes
        .windows(4)
        .enumerate()
        .filter_map(|(type_start, window)| {
            if window != box_type {
                return None;
            }
            let start = type_start.checked_sub(4)?;
            let size = usize::try_from(read_u32(bytes, start)).ok()?;
            (size >= 8 && start.checked_add(size)? <= bytes.len()).then_some(start)
        })
        .next_back()
        .expect("expected ISO box")
}

/// Читает big-endian `u32` из checked-in fixture.
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("test field fits input"),
    )
}
