//! End-to-end proofs на единственном каноническом Smooth/PIFF corpus.

use bounded_xml_reader::XmlBudgets;
use smooth_streaming_fmp4::{
    SmoothFragmentIndex, SmoothFragmentPlanRequest, SmoothFragmentReconstructionError,
    SmoothFragmentReconstructionRequest, SmoothInitializationError, SmoothInitializationRequest,
    SmoothReconstructedFragment, SmoothStreamOrdinal, SmoothTrackMappingError,
    SmoothTrackMappingRequest, SmoothTrackMediaKind, SmoothTrackSelection,
    build_smooth_initialization_segment, map_smooth_track, plan_smooth_fragment,
    reconstruct_smooth_fragment,
};
use smooth_streaming_manifest_core::{
    SmoothCodecConfigurationOrigin, SmoothManifest, SmoothManifestLimits,
    SmoothManifestParseRequest, SmoothQualityLevel, SmoothStreamKind, parse_vod_client_manifest,
};
use symphonia_format_isomp4::{
    FragmentInitializationLimitKind, FragmentInitializationLimits, FragmentInspectionLimitKind,
    FragmentInspectionLimits, FragmentWriteLimits,
};

/// Канонический manifest не копируется в adapter crate.
const MANIFEST: &[u8] =
    include_bytes!("../../symphonia-format-isomp4-patch/fixtures/smooth-piff/tears-of-steel.ismc");
/// Video low, первый fragment.
const VIDEO_LOW_FIRST: &[u8] =
    include_bytes!("../../symphonia-format-isomp4-patch/fixtures/smooth-piff/video-401000-0.bin");
/// Video high, первый fragment.
const VIDEO_HIGH_FIRST: &[u8] =
    include_bytes!("../../symphonia-format-isomp4-patch/fixtures/smooth-piff/video-1501000-0.bin");
/// Video high, второй fragment.
const VIDEO_HIGH_SECOND: &[u8] = include_bytes!(
    "../../symphonia-format-isomp4-patch/fixtures/smooth-piff/video-1501000-40000000.bin"
);
/// Audio low, первый fragment.
const AUDIO_FIRST: &[u8] =
    include_bytes!("../../symphonia-format-isomp4-patch/fixtures/smooth-piff/audio-64008-0.bin");
/// Audio low, второй fragment.
const AUDIO_SECOND: &[u8] = include_bytes!(
    "../../symphonia-format-isomp4-patch/fixtures/smooth-piff/audio-64008-39680000.bin"
);

/// Bounded XML budgets для ровно одного канонического документа.
fn xml_budgets() -> XmlBudgets {
    XmlBudgets::builder()
        .maximum_document_bytes(64 * 1024)
        .maximum_depth(8)
        .maximum_tokens(4_096)
        .maximum_attributes_per_element(16)
        .maximum_attribute_count(4_096)
        .maximum_attribute_bytes(32 * 1024)
        .maximum_namespace_declarations_per_element(4)
        .maximum_namespace_declaration_count(16)
        .maximum_namespace_bytes(1_024)
        .maximum_text_bytes(8 * 1024)
        .build()
        .expect("test XML budgets полны")
}

/// Manifest budgets покрывают corpus, но не являются production defaults.
fn manifest_limits() -> SmoothManifestLimits {
    SmoothManifestLimits::builder()
        .maximum_streams(8)
        .maximum_qualities_per_stream(16)
        .maximum_total_qualities(32)
        .maximum_timeline_entries_per_stream(512)
        .maximum_total_timeline_entries(1_024)
        .maximum_fragments_per_stream(512)
        .maximum_total_fragments(1_024)
        .maximum_template_bytes(512)
        .maximum_string_bytes(256)
        .maximum_codec_bytes(4_096)
        .maximum_custom_attributes_per_quality(8)
        .maximum_total_custom_attributes(32)
        .maximum_custom_attribute_name_bytes(64)
        .maximum_custom_attribute_value_bytes(128)
        .build()
        .expect("test manifest budgets полны")
}

/// Парсит caller-provided document через exact parser fixture.
fn parse_manifest(document: &[u8]) -> SmoothManifest {
    parse_vod_client_manifest(SmoothManifestParseRequest {
        document_bytes: document,
        xml_budgets: xml_budgets(),
        limits: manifest_limits(),
    })
    .expect("канонический Smooth manifest должен парситься")
}

/// Находит stream и quality по kind/bitrate, не предполагая video-first ordering.
fn selection_for(
    manifest: &SmoothManifest,
    media_kind: SmoothStreamKind,
    bitrate: u64,
) -> SmoothTrackSelection {
    manifest
        .streams()
        .iter()
        .enumerate()
        .find_map(|(stream_ordinal, stream)| {
            if stream.kind() != media_kind {
                return None;
            }
            stream.qualities().iter().find_map(|quality| {
                let quality_bitrate = match quality {
                    SmoothQualityLevel::Video(video) => video.bitrate().get(),
                    SmoothQualityLevel::Audio(audio) => audio.bitrate().get(),
                };
                (quality_bitrate == bitrate).then(|| {
                    SmoothTrackSelection::new(
                        SmoothStreamOrdinal::new(stream_ordinal),
                        quality.index(),
                    )
                })
            })
        })
        .expect("fixture stream/quality должен существовать")
}

/// Обязательные F1 init budgets передаются каждым caller-ом явно.
fn initialization_limits() -> FragmentInitializationLimits {
    FragmentInitializationLimits::builder()
        .maximum_output_bytes(16 * 1024)
        .maximum_codec_configuration_bytes(4 * 1024)
        .build()
        .expect("test init budgets полны")
}

/// Обязательные F1 inspection budgets передаются без adapter defaults.
fn inspection_limits() -> FragmentInspectionLimits {
    FragmentInspectionLimits::builder()
        .max_input_bytes(512 * 1024)
        .max_box_count(128)
        .max_box_depth(8)
        .max_traf_count(1)
        .max_trun_count(8)
        .max_samples(4_096)
        .max_sample_table_bytes(256 * 1024)
        .max_box_payload_bytes(512 * 1024)
        .build()
        .expect("test inspection budgets полны")
}

/// Обязательный F1 write budget.
fn write_limits() -> FragmentWriteLimits {
    FragmentWriteLimits::try_new(512 * 1024).expect("test write budget ненулевой")
}

/// Ищет ISO box по type в каноническом init output.
fn box_start(bytes: &[u8], box_type: [u8; 4]) -> usize {
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
        .expect("ожидаемый ISO box должен существовать")
}

/// Читает big-endian `u32`.
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("test field помещается в input"),
    )
}

/// Доказывает track ID/timescale/video dimensions и два разных H.264 config-а.
#[test]
fn video_mapping_and_initialization_are_exact_and_deterministic() {
    let manifest = parse_manifest(MANIFEST);
    let not_cancelled = || false;
    let init_limits = initialization_limits();
    let cases = [(401_000, 224_u16, 100_u16), (1_501_000, 1_680_u16, 750_u16)];

    for (bitrate, expected_width, expected_height) in cases {
        let track = map_smooth_track(SmoothTrackMappingRequest::new(
            &manifest,
            selection_for(&manifest, SmoothStreamKind::Video, bitrate),
            &not_cancelled,
        ))
        .expect("video mapping должен пройти");
        assert_eq!(track.identity().media_kind(), SmoothTrackMediaKind::Video);
        assert_eq!(track.identity().bitrate().get(), bitrate);
        assert_eq!(track.timescale_ticks_per_second(), 10_000_000);

        let first = build_smooth_initialization_segment(SmoothInitializationRequest::new(
            &track,
            &init_limits,
            &not_cancelled,
        ))
        .expect("video init должен собраться");
        let second = build_smooth_initialization_segment(SmoothInitializationRequest::new(
            &track,
            &init_limits,
            &not_cancelled,
        ))
        .expect("повторный video init должен собраться");
        assert_eq!(
            first.initialization_segment_bytes(),
            second.initialization_segment_bytes()
        );

        let bytes = first.initialization_segment_bytes();
        let tkhd = box_start(bytes, *b"tkhd");
        let mdhd = box_start(bytes, *b"mdhd");
        let avc1 = box_start(bytes, *b"avc1");
        assert_eq!(read_u32(bytes, tkhd + 20), 1);
        assert_eq!(read_u32(bytes, mdhd + 20), 10_000_000);
        assert_eq!(
            u16::from_be_bytes(bytes[avc1 + 32..avc1 + 34].try_into().unwrap()),
            expected_width
        );
        assert_eq!(
            u16::from_be_bytes(bytes[avc1 + 34..avc1 + 36].try_into().unwrap()),
            expected_height
        );
    }
}

/// Доказывает единый точный F1 track ID для independent video/audio resources.
#[test]
fn mapped_video_and_audio_expose_authoritative_reconstructed_track_id() {
    let manifest = parse_manifest(MANIFEST);
    let not_cancelled = || false;

    for (media_kind, bitrate) in [
        (SmoothStreamKind::Video, 401_000),
        (SmoothStreamKind::Audio, 64_008),
    ] {
        let track = map_smooth_track(SmoothTrackMappingRequest::new(
            &manifest,
            selection_for(&manifest, media_kind, bitrate),
            &not_cancelled,
        ))
        .expect("canonical track mapping должен пройти");

        assert_eq!(track.reconstructed_track_id().get(), 1);
    }
}

/// Доказывает exact ASC `11 90`, 48 kHz/stereo и derived manifest bytes.
#[test]
fn audio_mapping_preserves_explicit_and_derived_manifest_asc() {
    let explicit_manifest = parse_manifest(MANIFEST);
    let derived_document = String::from_utf8(MANIFEST.to_vec())
        .expect("fixture UTF-8")
        .replacen("      CodecPrivateData=\"1190\"\n", "", 1);
    let derived_manifest = parse_manifest(derived_document.as_bytes());
    let not_cancelled = || false;
    let init_limits = initialization_limits();

    for manifest in [&explicit_manifest, &derived_manifest] {
        let audio_stream = manifest
            .streams()
            .iter()
            .find(|stream| stream.kind() == SmoothStreamKind::Audio)
            .expect("audio stream");
        let audio_quality = match &audio_stream.qualities()[0] {
            SmoothQualityLevel::Audio(audio) => audio,
            SmoothQualityLevel::Video(_) => panic!("audio stream обязан содержать audio quality"),
        };
        assert_eq!(audio_quality.codec_configuration().as_bytes(), [0x11, 0x90]);
        assert_eq!(audio_quality.sampling_rate().get(), 48_000);
        assert_eq!(audio_quality.channels().get(), 2);
        if core::ptr::eq(manifest, &derived_manifest) {
            assert_eq!(
                audio_quality.codec_configuration().origin(),
                SmoothCodecConfigurationOrigin::AacDerivedFromQualityFields
            );
        }

        let track = map_smooth_track(SmoothTrackMappingRequest::new(
            manifest,
            selection_for(manifest, SmoothStreamKind::Audio, 64_008),
            &not_cancelled,
        ))
        .expect("audio mapping должен пройти");
        let initialization = build_smooth_initialization_segment(SmoothInitializationRequest::new(
            &track,
            &init_limits,
            &not_cancelled,
        ))
        .expect("audio init должен собраться");
        let bytes = initialization.initialization_segment_bytes();
        let tkhd = box_start(bytes, *b"tkhd");
        let mdhd = box_start(bytes, *b"mdhd");
        let mp4a = box_start(bytes, *b"mp4a");
        assert_eq!(read_u32(bytes, tkhd + 20), 1);
        assert_eq!(read_u32(bytes, mdhd + 20), 10_000_000);
        assert_eq!(
            u16::from_be_bytes(bytes[mp4a + 24..mp4a + 26].try_into().unwrap()),
            2
        );
        assert_eq!(read_u32(bytes, mp4a + 32), 48_000 << 16);
        assert!(
            bytes.windows(2).any(|window| window == [0x11, 0x90]),
            "init обязан содержать exact manifest ASC"
        );
    }
}

/// Reversed SPS/PPS остаётся допустимым, но ни start code, ни extra NAL не протекают в F1.
#[test]
fn reversed_canonical_sps_pps_order_is_accepted() {
    let original = String::from_utf8(MANIFEST.to_vec()).expect("fixture UTF-8");
    let video_start = original.find("Type=\"video\"").expect("video stream");
    let configuration_marker = "CodecPrivateData=\"";
    let configuration_start = video_start
        + original[video_start..]
            .find(configuration_marker)
            .expect("video codec configuration")
        + configuration_marker.len();
    let configuration_end = configuration_start
        + original[configuration_start..]
            .find('"')
            .expect("codec closing quote");
    let configuration = &original[configuration_start..configuration_end];
    let second_start_code = configuration[8..]
        .find("00000001")
        .map(|offset| offset + 8)
        .expect("second canonical start code");
    let reversed = format!(
        "{}{}{}{}",
        &original[..configuration_start],
        &configuration[second_start_code..],
        &configuration[..second_start_code],
        &original[configuration_end..]
    );
    let manifest = parse_manifest(reversed.as_bytes());
    let not_cancelled = || false;
    let track = map_smooth_track(SmoothTrackMappingRequest::new(
        &manifest,
        selection_for(&manifest, SmoothStreamKind::Video, 401_000),
        &not_cancelled,
    ))
    .expect("reversed SPS/PPS mapping должен пройти");
    build_smooth_initialization_segment(SmoothInitializationRequest::new(
        &track,
        &initialization_limits(),
        &not_cancelled,
    ))
    .expect("reversed SPS/PPS init должен собраться");
}

/// План строит exact path/window и сохраняет исчерпывающий video/audio kind.
#[test]
fn fragment_plans_render_exact_paths_and_windows() {
    let manifest = parse_manifest(MANIFEST);
    let not_cancelled = || false;
    let cases = [
        (
            SmoothStreamKind::Video,
            1_501_000,
            0,
            "QualityLevels(1501000)/Fragments(video_eng=0)",
            (0, 40_000_000),
        ),
        (
            SmoothStreamKind::Video,
            1_501_000,
            1,
            "QualityLevels(1501000)/Fragments(video_eng=40000000)",
            (40_000_000, 80_000_000),
        ),
        (
            SmoothStreamKind::Audio,
            64_008,
            0,
            "QualityLevels(64008)/Fragments(audio_eng=0)",
            (0, 39_680_000),
        ),
        (
            SmoothStreamKind::Audio,
            64_008,
            1,
            "QualityLevels(64008)/Fragments(audio_eng=39680000)",
            (39_680_000, 79_573_333),
        ),
    ];

    for (kind, bitrate, index, expected_path, expected_window) in cases {
        let expected_media_kind = match kind {
            SmoothStreamKind::Video => SmoothTrackMediaKind::Video,
            SmoothStreamKind::Audio => SmoothTrackMediaKind::Audio,
        };
        let track = map_smooth_track(SmoothTrackMappingRequest::new(
            &manifest,
            selection_for(&manifest, kind, bitrate),
            &not_cancelled,
        ))
        .expect("mapping должен пройти");
        let first = plan_smooth_fragment(SmoothFragmentPlanRequest::new(
            &track,
            SmoothFragmentIndex::new(index),
            &not_cancelled,
        ))
        .expect("plan должен собраться");
        let second = plan_smooth_fragment(SmoothFragmentPlanRequest::new(
            &track,
            SmoothFragmentIndex::new(index),
            &not_cancelled,
        ))
        .expect("повторный plan должен собраться");
        assert_eq!(first.identity().media_kind(), expected_media_kind);
        assert_eq!(second.identity().media_kind(), expected_media_kind);
        assert_eq!(
            first.relative_path().transport_relative_path(),
            expected_path
        );
        assert_eq!(
            first.relative_path().transport_relative_path(),
            second.relative_path().transport_relative_path()
        );
        assert_eq!(
            (
                first.manifest_window().start(),
                first.manifest_window().end_exclusive()
            ),
            expected_window
        );
        assert_eq!(
            format!("{:?}", first.relative_path()),
            format!(
                "SmoothFragmentRelativePath {{ byte_length: {} }}",
                expected_path.len()
            )
        );
        assert!(!format!("{first:?}").contains(expected_path));
    }
}

/// Три video fixtures exact и admitted; два audio fixtures retain exact pending proof.
#[test]
fn canonical_fragments_follow_exact_admission_matrix() {
    let manifest = parse_manifest(MANIFEST);
    let not_cancelled = || false;
    let inspection = inspection_limits();
    let video_cases = [
        (401_000, 0, VIDEO_LOW_FIRST, (0, 40_000_000)),
        (1_501_000, 0, VIDEO_HIGH_FIRST, (0, 40_000_000)),
        (1_501_000, 1, VIDEO_HIGH_SECOND, (40_000_000, 80_000_000)),
    ];

    for (bitrate, index, input, expected_coverage) in video_cases {
        let track = map_smooth_track(SmoothTrackMappingRequest::new(
            &manifest,
            selection_for(&manifest, SmoothStreamKind::Video, bitrate),
            &not_cancelled,
        ))
        .unwrap();
        let plan = plan_smooth_fragment(SmoothFragmentPlanRequest::new(
            &track,
            SmoothFragmentIndex::new(index),
            &not_cancelled,
        ))
        .unwrap();
        let outcome = reconstruct_smooth_fragment(SmoothFragmentReconstructionRequest::new(
            input,
            &plan,
            &inspection,
            write_limits(),
            &not_cancelled,
        ))
        .expect("exact video должен admission");
        let SmoothReconstructedFragment::Admitted(fragment) = outcome else {
            panic!("video exact обязан быть admitted");
        };
        assert_eq!(
            (
                fragment.coded_coverage().start(),
                fragment.coded_coverage().end_exclusive()
            ),
            expected_coverage
        );
        assert_eq!(fragment.identity(), track.identity());
    }

    let audio_cases = [
        (0, AUDIO_FIRST, (0, 40_106_666), 426_666, (0, 39_680_000)),
        (
            1,
            AUDIO_SECOND,
            (39_680_000, 79_573_334),
            1,
            (39_680_000, 79_573_333),
        ),
    ];
    for (index, input, expected_coverage, expected_excess, expected_window) in audio_cases {
        let track = map_smooth_track(SmoothTrackMappingRequest::new(
            &manifest,
            selection_for(&manifest, SmoothStreamKind::Audio, 64_008),
            &not_cancelled,
        ))
        .unwrap();
        let plan = plan_smooth_fragment(SmoothFragmentPlanRequest::new(
            &track,
            SmoothFragmentIndex::new(index),
            &not_cancelled,
        ))
        .unwrap();
        let first = reconstruct_smooth_fragment(SmoothFragmentReconstructionRequest::new(
            input,
            &plan,
            &inspection,
            write_limits(),
            &not_cancelled,
        ))
        .expect("audio overhang должен стать pending");
        let second = reconstruct_smooth_fragment(SmoothFragmentReconstructionRequest::new(
            input,
            &plan,
            &inspection,
            write_limits(),
            &not_cancelled,
        ))
        .expect("повторный audio reconstruction должен пройти");
        let SmoothReconstructedFragment::PendingExactAudioClipping(first) = first else {
            panic!("audio overhang обязан требовать exact clipping");
        };
        let SmoothReconstructedFragment::PendingExactAudioClipping(second) = second else {
            panic!("audio overhang обязан детерминированно оставаться pending");
        };
        assert_eq!(
            (
                first.coded_coverage().start(),
                first.coded_coverage().end_exclusive()
            ),
            expected_coverage
        );
        assert_eq!(
            (
                first.manifest_window().start(),
                first.manifest_window().end_exclusive()
            ),
            expected_window
        );
        assert_eq!(first.excess_ticks().get(), expected_excess);
        assert_eq!(first.timescale_ticks_per_second(), 10_000_000);
        assert_eq!(first.sample_rate_hz(), 48_000);
        assert_eq!(first.channel_count(), 2);
        assert_eq!(
            first.identity().media_kind(),
            SmoothTrackMediaKind::Audio,
            "pending path доступен только audio plan state"
        );
        assert_eq!(first.identity(), track.identity());
        assert_eq!(
            first.unchanged_media_segment_bytes(),
            second.unchanged_media_segment_bytes()
        );
    }
}

/// Находит первый box данного type в media fixture.
fn media_box_start(bytes: &[u8], box_type: [u8; 4]) -> usize {
    box_start(bytes, box_type)
}

/// Меняет последний explicit sample duration, сохраняя structural layout.
fn adjust_last_trun_duration(bytes: &mut [u8], delta: i32) {
    let trun = media_box_start(bytes, *b"trun");
    let flags = read_u32(bytes, trun + 8) & 0x00ff_ffff;
    assert_ne!(flags & 0x000100, 0, "fixture обязан иметь sample duration");
    let sample_count = usize::try_from(read_u32(bytes, trun + 12)).unwrap();
    let mut rows_start = trun + 16;
    if flags & 0x000001 != 0 {
        rows_start += 4;
    }
    if flags & 0x000004 != 0 {
        rows_start += 4;
    }
    let fields = [0x000100_u32, 0x000200, 0x000400, 0x000800];
    let active_fields: Vec<u32> = fields
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

/// Вставляет optional `tfdt` v1 и чинит `trun.data_offset`.
fn insert_mismatching_tfdt(bytes: &mut Vec<u8>) {
    let moof = media_box_start(bytes, *b"moof");
    let traf = media_box_start(bytes, *b"traf");
    let traf_end = traf + usize::try_from(read_u32(bytes, traf)).unwrap();
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
    let changed_moof_end = moof + usize::try_from(read_u32(bytes, moof)).unwrap();
    let trun = media_box_start(bytes, *b"trun");
    let data_offset = u32::try_from(changed_moof_end + 8 - moof).unwrap();
    bytes[trun + 16..trun + 20].copy_from_slice(&data_offset.to_be_bytes());
}

/// Underrun, video overhang и start mismatch никогда не становятся admitted/pending.
#[test]
fn classifier_rejects_underrun_video_overhang_and_start_mismatch() {
    let manifest = parse_manifest(MANIFEST);
    let not_cancelled = || false;
    let inspection = inspection_limits();
    let track = map_smooth_track(SmoothTrackMappingRequest::new(
        &manifest,
        selection_for(&manifest, SmoothStreamKind::Video, 1_501_000),
        &not_cancelled,
    ))
    .unwrap();
    let plan = plan_smooth_fragment(SmoothFragmentPlanRequest::new(
        &track,
        SmoothFragmentIndex::new(0),
        &not_cancelled,
    ))
    .unwrap();

    let mut underrun = VIDEO_HIGH_FIRST.to_vec();
    adjust_last_trun_duration(&mut underrun, -1);
    let error = reconstruct_smooth_fragment(SmoothFragmentReconstructionRequest::new(
        &underrun,
        &plan,
        &inspection,
        write_limits(),
        &not_cancelled,
    ))
    .expect_err("underrun должен быть отвергнут");
    assert!(matches!(
        error,
        SmoothFragmentReconstructionError::Underrun { missing_ticks }
            if missing_ticks.get() == 1
    ));

    let mut overhang = VIDEO_HIGH_FIRST.to_vec();
    adjust_last_trun_duration(&mut overhang, 1);
    let error = reconstruct_smooth_fragment(SmoothFragmentReconstructionRequest::new(
        &overhang,
        &plan,
        &inspection,
        write_limits(),
        &not_cancelled,
    ))
    .expect_err("video overhang должен быть отвергнут");
    assert!(matches!(
        error,
        SmoothFragmentReconstructionError::VideoOverhang { excess_ticks }
            if excess_ticks.get() == 1
    ));

    let mut start_mismatch = VIDEO_HIGH_FIRST.to_vec();
    insert_mismatching_tfdt(&mut start_mismatch);
    let error = reconstruct_smooth_fragment(SmoothFragmentReconstructionRequest::new(
        &start_mismatch,
        &plan,
        &inspection,
        write_limits(),
        &not_cancelled,
    ))
    .expect_err("tfdt mismatch должен быть отвергнут");
    assert!(matches!(
        error,
        SmoothFragmentReconstructionError::StartMismatch {
            expected_start: 0,
            actual_start: 1
        }
    ));
}

/// Budgets и cancellation остаются явными на каждой стадии.
#[test]
fn stages_honor_cancellation_budgets_and_redaction() {
    let manifest = parse_manifest(MANIFEST);
    let cancelled = || true;
    let not_cancelled = || false;
    let selection = selection_for(&manifest, SmoothStreamKind::Video, 1_501_000);
    assert!(matches!(
        map_smooth_track(SmoothTrackMappingRequest::new(
            &manifest, selection, &cancelled
        )),
        Err(SmoothTrackMappingError::Cancelled)
    ));
    let track = map_smooth_track(SmoothTrackMappingRequest::new(
        &manifest,
        selection,
        &not_cancelled,
    ))
    .unwrap();
    assert!(matches!(
        build_smooth_initialization_segment(SmoothInitializationRequest::new(
            &track,
            &initialization_limits(),
            &cancelled
        )),
        Err(SmoothInitializationError::Cancelled)
    ));
    assert!(
        plan_smooth_fragment(SmoothFragmentPlanRequest::new(
            &track,
            SmoothFragmentIndex::new(0),
            &cancelled,
        ))
        .is_err()
    );
    let plan = plan_smooth_fragment(SmoothFragmentPlanRequest::new(
        &track,
        SmoothFragmentIndex::new(0),
        &not_cancelled,
    ))
    .unwrap();
    assert!(matches!(
        reconstruct_smooth_fragment(SmoothFragmentReconstructionRequest::new(
            VIDEO_HIGH_FIRST,
            &plan,
            &inspection_limits(),
            write_limits(),
            &cancelled,
        )),
        Err(SmoothFragmentReconstructionError::Cancelled)
    ));

    let tiny_init_limits = FragmentInitializationLimits::builder()
        .maximum_output_bytes(1)
        .maximum_codec_configuration_bytes(4 * 1024)
        .build()
        .unwrap();
    let error = build_smooth_initialization_segment(SmoothInitializationRequest::new(
        &track,
        &tiny_init_limits,
        &not_cancelled,
    ))
    .expect_err("tiny init budget должен сработать");
    assert!(matches!(
        error,
        SmoothInitializationError::Contract(
            symphonia_format_isomp4::FragmentInitializationError::LimitExceeded {
                kind: FragmentInitializationLimitKind::OutputBytes,
                ..
            }
        )
    ));

    let tiny_inspection = FragmentInspectionLimits::builder()
        .max_input_bytes(1)
        .max_box_count(128)
        .max_box_depth(8)
        .max_traf_count(1)
        .max_trun_count(8)
        .max_samples(4_096)
        .max_sample_table_bytes(256 * 1024)
        .max_box_payload_bytes(512 * 1024)
        .build()
        .unwrap();
    let error = reconstruct_smooth_fragment(SmoothFragmentReconstructionRequest::new(
        VIDEO_HIGH_FIRST,
        &plan,
        &tiny_inspection,
        write_limits(),
        &not_cancelled,
    ))
    .expect_err("tiny inspection budget должен сработать");
    assert!(matches!(
        error,
        SmoothFragmentReconstructionError::Inspection(
            symphonia_format_isomp4::FragmentInspectionError::LimitExceeded {
                kind: FragmentInspectionLimitKind::InputBytes,
                ..
            }
        )
    ));
    let debug_error = format!("{error:?}");
    assert!(!debug_error.contains("QualityLevels("));
    assert!(!debug_error.contains("0000000167"));
}
