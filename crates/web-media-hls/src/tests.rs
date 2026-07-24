use std::num::{NonZeroU16, NonZeroU32, NonZeroUsize};

use hls_playlist_core::{
    HlsParseRequest, HlsParserLimits, HlsPlaylist, MediaPlaylist, parse_hls_playlist,
};
use media_core::{TrackId, TrackInfo, TrackKind};
use source_core::HttpRequestTarget;

use crate::open::{HlsVodOpenError, codec_sets_match, select_master, validate_track_shape};
use crate::plan::{
    HlsPlanError, PlannedKeySource, build_component_plan, build_segment_scoped_component_plan,
};
use crate::{
    ExtractorAesOverride, HlsAudioLayoutIntent, HlsAudioRenditionEvidence,
    HlsMainTrackLayoutIntent, HlsRequestOverrides, HlsRequiredContainer, HlsVariantSelectionIntent,
};

fn parse_media(document: &str) -> MediaPlaylist {
    let playlist = parse_hls_playlist(HlsParseRequest {
        document_bytes: document.as_bytes(),
        reference_base: Some("https://media.invalid/root/master.m3u8"),
        limits: HlsParserLimits::default(),
    })
    .expect("valid fixture");
    let HlsPlaylist::Media(media) = playlist else {
        panic!("expected media playlist");
    };
    media
}

fn track(id: u32, kind: TrackKind) -> TrackInfo {
    TrackInfo {
        id: TrackId::new(id),
        kind,
        codec_id: "fixture".to_owned(),
        codec_private: None,
        time_base: None,
        duration: None,
        sample_rate: None,
        channels: None,
        video: None,
    }
}

fn parse_master(document: &str) -> hls_playlist_core::MasterPlaylist {
    let playlist = parse_hls_playlist(HlsParseRequest {
        document_bytes: document.as_bytes(),
        reference_base: Some("https://media.invalid/root/master.m3u8"),
        limits: HlsParserLimits::default(),
    })
    .expect("valid fixture");
    let HlsPlaylist::Master(master) = playlist else {
        panic!("expected master playlist");
    };
    master
}

fn base() -> HttpRequestTarget {
    HttpRequestTarget::parse_exact("https://media.invalid/root/media.m3u8?manifest=1")
        .expect("base")
}

#[test]
fn byte_ranges_are_transactional_for_explicit_fmp4_intent() {
    let media = parse_media(
        "#EXTM3U\n\
         #EXT-X-TARGETDURATION:4\n\
         #EXT-X-MAP:URI=\"shared.mp4?kept=1\",BYTERANGE=\"8@0\"\n\
         #EXTINF:4,\n\
         #EXT-X-BYTERANGE:4\n\
         shared.mp4?kept=1\n\
         #EXTINF:4,\n\
         #EXT-X-BYTERANGE:4\n\
         shared.mp4?kept=1\n\
         #EXT-X-ENDLIST\n",
    );
    let plan = build_component_plan(
        &media,
        HlsRequiredContainer::FragmentedMp4,
        &base(),
        &HlsRequestOverrides::new(None),
    )
    .expect("range plan");
    assert_eq!(plan.container, HlsRequiredContainer::FragmentedMp4);
    assert_eq!(plan.epochs.len(), 1);
    let resources = &plan.epochs[0].resources;
    assert_eq!(resources.len(), 3);
    assert_eq!(resources[0].byte_range.expect("map range").start(), 0);
    assert_eq!(
        resources[1].byte_range.expect("first media range").start(),
        8
    );
    assert_eq!(
        resources[2].byte_range.expect("second media range").start(),
        12
    );
    assert!(
        resources[1]
            .target
            .expose_secret_for_request()
            .contains("kept=1")
    );
    let bound_error = plan
        .validate_resource_bound(NonZeroUsize::new(3).expect("small bound"))
        .expect_err("known range exceeds bound");
    assert!(matches!(
        bound_error,
        HlsPlanError::ResourceRangeExceedsAdaptiveLimit
    ));
}

#[test]
fn implicit_range_rejects_a_different_effective_resource() {
    let media = parse_media(
        "#EXTM3U\n\
         #EXT-X-TARGETDURATION:4\n\
         #EXTINF:4,\n\
         #EXT-X-BYTERANGE:4@0\n\
         first.ts\n\
         #EXTINF:4,\n\
         #EXT-X-BYTERANGE:4\n\
         second.ts\n\
         #EXT-X-ENDLIST\n",
    );
    let error = build_component_plan(
        &media,
        HlsRequiredContainer::TransportStream,
        &base(),
        &HlsRequestOverrides::new(None),
    )
    .expect_err("implicit range must retain effective resource");
    assert!(matches!(error, HlsPlanError::MissingImplicitByteRangeBase));
}

#[test]
fn explicit_fmp4_intent_requires_map_while_ts_intent_accepts_map() {
    let without_map = parse_media(
        "#EXTM3U\n#EXT-X-TARGETDURATION:1\n\
         #EXTINF:1,\nsegment.m4s\n#EXT-X-ENDLIST\n",
    );
    let error = build_component_plan(
        &without_map,
        HlsRequiredContainer::FragmentedMp4,
        &base(),
        &HlsRequestOverrides::new(None),
    )
    .expect_err("fMP4 requires MAP");
    assert!(matches!(error, HlsPlanError::FragmentedMp4MapRequired));

    let ts_with_map = parse_media(
        "#EXTM3U\n#EXT-X-TARGETDURATION:1\n\
         #EXT-X-MAP:URI=\"header.ts\"\n\
         #EXTINF:1,\nsegment.ts\n#EXT-X-ENDLIST\n",
    );
    let plan = build_component_plan(
        &ts_with_map,
        HlsRequiredContainer::TransportStream,
        &base(),
        &HlsRequestOverrides::new(None),
    )
    .expect("TS MAP is structurally valid");
    assert_eq!(
        plan.epochs[0].resources[0].kind,
        demux_api::OrderedSegmentKind::Initialization
    );
}

#[test]
fn aes_override_is_active_only_for_encrypted_resources_and_replaces_key_target() {
    let media = parse_media(
        "#EXTM3U\n\
         #EXT-X-TARGETDURATION:4\n\
         #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x1\n\
         #EXTINF:4,\n\
         encrypted.ts\n\
         #EXT-X-KEY:METHOD=NONE\n\
         #EXTINF:4,\n\
         clear.ts\n\
         #EXT-X-ENDLIST\n",
    );
    let plan = build_component_plan(
        &media,
        HlsRequiredContainer::TransportStream,
        &base(),
        &HlsRequestOverrides::new(None),
    )
    .expect("AES plan");
    let encrypted = plan.epochs[0].resources[0]
        .encryption
        .as_ref()
        .expect("active encryption");
    let PlannedKeySource::ManifestTarget(key_target) = &encrypted.key else {
        panic!("manifest key target expected");
    };
    assert!(key_target.expose_secret_for_request().ends_with("/key.bin"));
    assert!(plan.epochs[0].resources[1].encryption.is_none());

    let override_plan = build_component_plan(
        &media,
        HlsRequiredContainer::TransportStream,
        &base(),
        &HlsRequestOverrides::new(Some(
            ExtractorAesOverride::new(
                Some("https://keys.invalid/replaced?exact=1"),
                None,
                Some("2"),
            )
            .expect("override"),
        )),
    )
    .expect("replacement plan");
    let replacement = override_plan.epochs[0].resources[0]
        .encryption
        .as_ref()
        .expect("replacement encryption");
    let PlannedKeySource::ExtractorReplacement(key_target) = &replacement.key else {
        panic!("replacement target expected");
    };
    assert_eq!(
        key_target.expose_secret_for_request(),
        "https://keys.invalid/replaced?exact=1"
    );
}

#[test]
fn discontinuity_and_map_change_create_monotonic_epochs() {
    let media = parse_media(
        "#EXTM3U\n\
         #EXT-X-TARGETDURATION:4\n\
         #EXT-X-MAP:URI=\"init-a.mp4\"\n\
         #EXTINF:4,\n\
         a.m4s\n\
         #EXT-X-DISCONTINUITY\n\
         #EXTINF:4,\n\
         b.m4s\n\
         #EXT-X-MAP:URI=\"init-b.mp4\"\n\
         #EXTINF:4,\n\
         c.m4s\n\
         #EXT-X-ENDLIST\n",
    );
    let plan = build_component_plan(
        &media,
        HlsRequiredContainer::FragmentedMp4,
        &base(),
        &HlsRequestOverrides::new(None),
    )
    .expect("epoch plan");
    assert_eq!(plan.epochs.len(), 3);
    assert_eq!(plan.epochs[0].timeline_start.as_secs(), 0);
    assert_eq!(plan.epochs[1].timeline_start.as_secs(), 4);
    assert_eq!(plan.epochs[2].timeline_start.as_secs(), 8);
}

#[test]
fn live_ts_and_fmp4_plans_keep_exact_segment_scoped_epochs() {
    let ts = parse_media(
        "#EXTM3U\n\
         #EXT-X-TARGETDURATION:4\n\
         #EXTINF:4,\n\
         a.ts\n\
         #EXTINF:4,\n\
         b.ts\n",
    );
    let ts_plan = build_segment_scoped_component_plan(
        &ts,
        HlsRequiredContainer::TransportStream,
        &base(),
        &HlsRequestOverrides::new(None),
    )
    .expect("segment-scoped TS plan");
    assert_eq!(ts_plan.epochs.len(), ts.segments.len());

    let fmp4 = parse_media(
        "#EXTM3U\n\
         #EXT-X-TARGETDURATION:4\n\
         #EXT-X-MAP:URI=\"init.mp4\"\n\
         #EXTINF:4,\n\
         a.m4s\n\
         #EXTINF:4,\n\
         b.m4s\n",
    );
    let fmp4_plan = build_segment_scoped_component_plan(
        &fmp4,
        HlsRequiredContainer::FragmentedMp4,
        &base(),
        &HlsRequestOverrides::new(None),
    )
    .expect("segment-scoped fMP4 plan");
    assert_eq!(fmp4_plan.epochs.len(), fmp4.segments.len());
    assert!(
        fmp4_plan
            .epochs
            .iter()
            .all(|epoch| epoch.resources.len() == 2)
    );
}

#[test]
fn strict_master_selection_uses_variant_audio_and_subtitle_evidence() {
    let master = parse_master(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,URI=\"audio-en.m3u8\"\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"French\",LANGUAGE=\"fr\",AUTOSELECT=YES,URI=\"audio-fr.m3u8\"\n\
         #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",LANGUAGE=\"en\",URI=\"sub-en.m3u8\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=1280x720,CODECS=\"avc1.4d401f\",AUDIO=\"aud\",SUBTITLES=\"subs\"\n\
         video-720.m3u8\n\
         #EXT-X-STREAM-INF:BANDWIDTH=2000000,RESOLUTION=1920x1080,CODECS=\"avc1.640028\",AUDIO=\"aud\",SUBTITLES=\"subs\"\n\
         video-1080.m3u8\n",
    );
    let selected = select_master(
        &master,
        &HlsVariantSelectionIntent {
            resolution: Some((
                NonZeroU32::new(1920).expect("width"),
                NonZeroU32::new(1080).expect("height"),
            )),
            codecs: Some("avc1.640028".into()),
            audio: HlsAudioLayoutIntent::Separate(HlsAudioRenditionEvidence {
                language: Some("en".into()),
                ..HlsAudioRenditionEvidence::default()
            }),
            main_track_layout: HlsMainTrackLayoutIntent::VideoOnly,
        },
    )
    .expect("strict selection");
    assert_eq!(
        selected.variant.uri.expose_for_resolution(),
        "video-1080.m3u8"
    );
    assert_eq!(selected.audio.expect("audio").name.as_ref(), "English");
    assert_eq!(selected.subtitles.len(), 1);
    assert_eq!(selected.subtitles[0].name(), "English");
}

#[test]
fn master_without_variant_evidence_is_typed_ambiguous() {
    let master = parse_master(
        "#EXTM3U\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1000000\n\
         one.m3u8\n\
         #EXT-X-STREAM-INF:BANDWIDTH=2000000\n\
         two.m3u8\n",
    );
    let error = select_master(
        &master,
        &HlsVariantSelectionIntent {
            resolution: None,
            codecs: None,
            audio: HlsAudioLayoutIntent::Muxed,
            main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
        },
    )
    .expect_err("ambiguous master");
    assert!(matches!(error, HlsVodOpenError::AmbiguousVariant));
}

#[test]
fn master_without_matching_variant_is_typed_missing() {
    let master = parse_master(
        "#EXTM3U\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=1280x720\n\
         one.m3u8\n",
    );
    let error = select_master(
        &master,
        &HlsVariantSelectionIntent {
            resolution: Some((
                NonZeroU32::new(1920).expect("width"),
                NonZeroU32::new(1080).expect("height"),
            )),
            codecs: None,
            audio: HlsAudioLayoutIntent::Muxed,
            main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
        },
    )
    .expect_err("missing exact variant");
    assert!(matches!(error, HlsVodOpenError::MissingVariant));
}

#[test]
fn default_and_autoselect_do_not_break_strict_audio_tie() {
    let master = parse_master(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"One\",DEFAULT=YES,AUTOSELECT=YES,URI=\"one.m3u8\"\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Two\",DEFAULT=NO,AUTOSELECT=YES,URI=\"two.m3u8\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1000,AUDIO=\"aud\"\n\
         video.m3u8\n",
    );
    let error = select_master(
        &master,
        &HlsVariantSelectionIntent {
            resolution: None,
            codecs: None,
            audio: HlsAudioLayoutIntent::ManifestResolved(HlsAudioRenditionEvidence::default()),
            main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
        },
    )
    .expect_err("DEFAULT/AUTOSELECT не являются exact evidence");
    assert!(matches!(error, HlsVodOpenError::AmbiguousAudioRendition));
}

#[test]
fn mixed_in_band_and_external_audio_group_is_typed_ambiguous() {
    let master = parse_master(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",LANGUAGE=\"en\"\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English External\",LANGUAGE=\"en\",URI=\"en.m3u8\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1000,AUDIO=\"aud\"\n\
         video.m3u8\n",
    );
    let error = select_master(
        &master,
        &HlsVariantSelectionIntent {
            resolution: None,
            codecs: None,
            audio: HlsAudioLayoutIntent::ManifestResolved(HlsAudioRenditionEvidence {
                language: Some("en".into()),
                ..HlsAudioRenditionEvidence::default()
            }),
            main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
        },
    )
    .expect_err("topology нельзя выбирать по наличию URI");
    assert!(matches!(error, HlsVodOpenError::AmbiguousAudioRendition));
}

#[test]
fn channel_count_evidence_matches_rfc_channels_primary_parameter() {
    let master = parse_master(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Atmos\",LANGUAGE=\"en\",CHANNELS=\"2/JOC\",URI=\"atmos.m3u8\"\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Surround\",LANGUAGE=\"en\",CHANNELS=\"6\",URI=\"surround.m3u8\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1000,AUDIO=\"aud\"\n\
         video.m3u8\n",
    );
    let selected = select_master(
        &master,
        &HlsVariantSelectionIntent {
            resolution: None,
            codecs: None,
            audio: HlsAudioLayoutIntent::Separate(HlsAudioRenditionEvidence {
                language: Some("en".into()),
                channel_count: NonZeroU16::new(2),
                ..HlsAudioRenditionEvidence::default()
            }),
            main_track_layout: HlsMainTrackLayoutIntent::VideoOnly,
        },
    )
    .expect("primary channel-count evidence selects the RFC CHANNELS rendition");
    assert_eq!(
        selected.audio.expect("selected audio").name.as_ref(),
        "Atmos"
    );
}

#[test]
fn main_track_layout_accepts_all_exact_shapes_and_rejects_opposite_tracks() {
    let video = track(1, TrackKind::Video);
    let audio = track(2, TrackKind::Audio);
    validate_track_shape(
        &[video.clone(), audio.clone()],
        HlsMainTrackLayoutIntent::MuxedAv,
        "muxed",
    )
    .expect("exact muxed A/V");
    validate_track_shape(
        std::slice::from_ref(&video),
        HlsMainTrackLayoutIntent::VideoOnly,
        "video",
    )
    .expect("exact video-only");
    validate_track_shape(
        std::slice::from_ref(&audio),
        HlsMainTrackLayoutIntent::AudioOnly,
        "audio",
    )
    .expect("exact audio-only");
    assert!(
        validate_track_shape(
            &[video.clone(), audio.clone()],
            HlsMainTrackLayoutIntent::VideoOnly,
            "video with unexpected audio",
        )
        .is_err()
    );
    assert!(
        validate_track_shape(
            &[video, audio],
            HlsMainTrackLayoutIntent::AudioOnly,
            "audio with unexpected video",
        )
        .is_err()
    );
}

#[test]
fn codec_evidence_is_exact_but_order_independent() {
    assert!(codec_sets_match(
        "avc1.640028,mp4a.40.2",
        "mp4a.40.2,avc1.640028"
    ));
    assert!(!codec_sets_match("avc1.640028", "avc1.4d401f"));
    assert!(!codec_sets_match("avc1.640028,mp4a.40.2", "avc1.640028"));
}

#[test]
fn runtime_request_and_plan_debug_remain_secret_safe() {
    let media = parse_media(
        "#EXTM3U\n\
         #EXT-X-TARGETDURATION:4\n\
         #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin?key-secret=1\"\n\
         #EXTINF:4,\n\
         segment.ts?segment-secret=1\n\
         #EXT-X-ENDLIST\n",
    );
    let overrides = HlsRequestOverrides::new(None);
    let plan = build_component_plan(
        &media,
        HlsRequiredContainer::TransportStream,
        &base(),
        &overrides,
    )
    .expect("secret-safe plan");
    let diagnostic = format!("{overrides:?} {plan:?}");
    for secret in ["segment-secret", "key-secret"] {
        assert!(!diagnostic.contains(secret), "{secret} leaked");
    }
}
