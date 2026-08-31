use std::collections::VecDeque;
use std::num::{NonZeroU16, NonZeroU32, NonZeroUsize};

use hls_playlist_core::{
    HlsParseRequest, HlsParserLimits, HlsPlaylist, MasterPlaylist, parse_hls_playlist,
};
use web_media_core::{
    AudioTrackDescriptor, CandidateFormatIdentity, CandidateIdentity,
    ComponentVariantCatalogGeneration, ComponentVariantCatalogIdentity,
    ComponentVariantCatalogLimit, ComponentVariantEdgeLimit, DynamicRange, ExactSelectionIdentity,
    ExtractionGeneration, NormalizedCodec, RawCodecIdentity, SemanticIdentity, SourceIdentity,
    VideoTrackDescriptor,
};

use super::*;
use crate::{
    HlsAudioLayoutIntent, HlsAudioRenditionEvidence, HlsMainTrackLayoutIntent,
    HlsVariantSelectionIntent,
};

struct ProofQueue {
    replies: VecDeque<Result<HlsCatalogChildProof, HlsCatalogChildProofError>>,
    calls: Vec<HlsCatalogChildProbe>,
}

impl ProofQueue {
    fn new(replies: impl IntoIterator<Item = HlsCatalogChildProof>) -> Self {
        Self {
            replies: replies.into_iter().map(Ok).collect(),
            calls: Vec::new(),
        }
    }
}

impl HlsCatalogChildProofPort for ProofQueue {
    fn prove_child(
        &mut self,
        request: HlsCatalogChildProbe,
    ) -> Result<HlsCatalogChildProof, HlsCatalogChildProofError> {
        self.calls.push(request);
        self.replies
            .pop_front()
            .expect("one queued proof per child")
    }
}

fn parse(text: &str) -> HlsPlaylist {
    parse_hls_playlist(HlsParseRequest::new(
        text.as_bytes(),
        Some("https://media.example.invalid/master.m3u8"),
        HlsParserLimits::default(),
    ))
    .expect("test playlist")
}

fn master(text: &str) -> MasterPlaylist {
    let HlsPlaylist::Master(master) = parse(text) else {
        panic!("expected master");
    };
    master
}

fn catalog_identity(extraction: u64, catalog: u64) -> ComponentVariantCatalogIdentity {
    let source = SourceIdentity::new(77);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(extraction),
        CandidateFormatIdentity::new(format!("parent-{extraction}")).expect("test exact identity"),
    );
    let semantic = SemanticIdentity::new(source, "hls-parent").expect("test semantic identity");
    ComponentVariantCatalogIdentity::new(
        ExactSelectionIdentity::new(exact, semantic).expect("same source"),
        ComponentVariantCatalogGeneration::new(catalog),
    )
}

fn policy() -> HlsCatalogBuildPolicy {
    HlsCatalogBuildPolicy {
        catalog_limit: ComponentVariantCatalogLimit::new(32).expect("catalog limit"),
        compatibility_edge_limit: ComponentVariantEdgeLimit::new(64).expect("edge limit"),
        maximum_unique_children: NonZeroUsize::new(16).expect("child limit"),
    }
}

fn video(dynamic_range: DynamicRange) -> VideoTrackDescriptor {
    VideoTrackDescriptor::new(
        NormalizedCodec::parse(RawCodecIdentity::new("avc1.640028").expect("codec")),
        None,
        None,
        None,
        None,
        dynamic_range,
    )
}

fn audio() -> AudioTrackDescriptor {
    AudioTrackDescriptor::new(
        NormalizedCodec::parse(RawCodecIdentity::new("mp4a.40.2").expect("codec")),
        None,
        None,
        None,
        None,
    )
}

fn muxed_proof() -> HlsCatalogChildProof {
    HlsCatalogChildProof {
        container: HlsRequiredContainer::FragmentedMp4,
        tracks: HlsCatalogTrackProof::Muxed {
            video: video(DynamicRange::Unknown),
            audio: audio(),
        },
        alignment: HlsCatalogAlignmentProof::new(1),
    }
}

fn video_proof() -> HlsCatalogChildProof {
    HlsCatalogChildProof {
        container: HlsRequiredContainer::TransportStream,
        tracks: HlsCatalogTrackProof::VideoOnly(video(DynamicRange::Unknown)),
        alignment: HlsCatalogAlignmentProof::new(1),
    }
}

fn audio_proof() -> HlsCatalogChildProof {
    audio_proof_with_alignment(1)
}

fn audio_proof_with_alignment(alignment: u64) -> HlsCatalogChildProof {
    HlsCatalogChildProof {
        container: HlsRequiredContainer::FragmentedMp4,
        tracks: HlsCatalogTrackProof::AudioOnly(audio()),
        alignment: HlsCatalogAlignmentProof::new(alignment),
    }
}

fn muxed_intent(width: u32, height: u32) -> HlsVariantSelectionIntent {
    HlsVariantSelectionIntent {
        resolution: Some((
            NonZeroU32::new(width).expect("width"),
            NonZeroU32::new(height).expect("height"),
        )),
        codecs: Some("avc1.640028,mp4a.40.2".into()),
        audio: HlsAudioLayoutIntent::Muxed,
        main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
    }
}

fn separate_intent(width: u32, height: u32, name: &str) -> HlsVariantSelectionIntent {
    HlsVariantSelectionIntent {
        resolution: Some((
            NonZeroU32::new(width).expect("width"),
            NonZeroU32::new(height).expect("height"),
        )),
        codecs: Some("avc1.640028,mp4a.40.2".into()),
        audio: HlsAudioLayoutIntent::Separate(HlsAudioRenditionEvidence {
            name: Some(name.into()),
            language: Some("en".into()),
            channel_count: Some(NonZeroU16::new(2).expect("channels")),
        }),
        main_track_layout: HlsMainTrackLayoutIntent::VideoOnly,
    }
}

#[test]
fn media_playlist_seed_has_no_fake_variant_inventory() {
    let playlist =
        parse("#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\nsegment.ts\n#EXT-X-ENDLIST\n");
    assert!(matches!(
        seed_hls_catalog_topology(&playlist),
        HlsCatalogTopologySeed::Unavailable
    ));
}

#[test]
fn muxed_variant_is_coupled_and_keeps_exact_manifest_facets() {
    let master = master(
        "#EXTM3U\n\
         #EXT-X-STREAM-INF:BANDWIDTH=9000000,CODECS=\"avc1.640028,mp4a.40.2\",RESOLUTION=1920x1080,FRAME-RATE=29.970,VIDEO-RANGE=PQ\n\
         muxed.m3u8?token=one\n",
    );
    let intent = muxed_intent(1920, 1080);
    let mut proofs = ProofQueue::new([muxed_proof()]);
    let snapshot = build_hls_catalog(
        HlsCatalogBuildRequest {
            master: &master,
            catalog_identity: catalog_identity(1, 1),
            provider_default: &intent,
            provider_default_variant_index: None,
            policy: policy(),
        },
        &mut proofs,
    )
    .expect("coupled catalog");

    assert_eq!(proofs.calls.len(), 1);
    assert_eq!(snapshot.catalog().coupled_presentations().len(), 1);
    let video = snapshot.catalog().coupled_presentations()[0].video();
    let frame_rate = video.frame_rate().expect("manifest frame rate");
    assert_eq!(
        (frame_rate.numerator(), frame_rate.denominator()),
        (2_997, 100)
    );
    assert_eq!(video.dynamic_range(), DynamicRange::Hdr);
    assert_eq!(
        video.bitrate(),
        None,
        "aggregate BANDWIDTH is not component bitrate"
    );
}

#[test]
fn external_audio_groups_create_sparse_edges_and_standalone_pools() {
    let master = master(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"English\",LANGUAGE=\"en\",CHANNELS=\"2\",URI=\"a.m3u8\"\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"b\",NAME=\"English\",LANGUAGE=\"en\",CHANNELS=\"2\",URI=\"b.m3u8\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.640028,mp4a.40.2\",RESOLUTION=1280x720,AUDIO=\"a\"\n\
         v720.m3u8\n\
         #EXT-X-STREAM-INF:BANDWIDTH=2,CODECS=\"avc1.640028,mp4a.40.2\",RESOLUTION=1920x1080,AUDIO=\"b\"\n\
         v1080.m3u8\n",
    );
    let intent = separate_intent(1280, 720, "English");
    let mut proofs = ProofQueue::new([video_proof(), video_proof(), audio_proof(), audio_proof()]);
    let snapshot = build_hls_catalog(
        HlsCatalogBuildRequest {
            master: &master,
            catalog_identity: catalog_identity(1, 1),
            provider_default: &intent,
            provider_default_variant_index: None,
            policy: policy(),
        },
        &mut proofs,
    )
    .expect("sparse catalog");

    assert_eq!(
        snapshot.catalog().required_video_variants().unwrap().len(),
        2
    );
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
        2
    );
    assert_eq!(snapshot.catalog().stored_variant_count(), 4);
}

#[test]
fn pure_video_and_audio_ladders_remain_standalone_without_fake_edges() {
    let master = master(
        "#EXTM3U\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.640028\",RESOLUTION=1280x720\nvideo.m3u8\n\
         #EXT-X-STREAM-INF:BANDWIDTH=2,CODECS=\"mp4a.40.2\"\naudio.m3u8\n",
    );
    let intent = HlsVariantSelectionIntent {
        resolution: Some((
            NonZeroU32::new(1280).unwrap(),
            NonZeroU32::new(720).unwrap(),
        )),
        codecs: Some("avc1.640028".into()),
        audio: HlsAudioLayoutIntent::Muxed,
        main_track_layout: HlsMainTrackLayoutIntent::VideoOnly,
    };
    let mut proofs = ProofQueue::new([video_proof(), audio_proof()]);
    let snapshot = build_hls_catalog(
        HlsCatalogBuildRequest {
            master: &master,
            catalog_identity: catalog_identity(1, 1),
            provider_default: &intent,
            provider_default_variant_index: None,
            policy: policy(),
        },
        &mut proofs,
    )
    .expect("standalone pools");
    assert_eq!(
        snapshot.catalog().required_video_variants().unwrap().len(),
        1
    );
    assert_eq!(
        snapshot.catalog().required_audio_variants().unwrap().len(),
        1
    );
    assert_eq!(
        snapshot
            .catalog()
            .compatibility()
            .expect("topology relation")
            .logical_edge_count(),
        0
    );
}

#[test]
fn uri_less_audio_is_only_a_coupled_main_presentation() {
    let master = master(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"embedded\",NAME=\"Main\",DEFAULT=YES,AUTOSELECT=YES\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.640028,mp4a.40.2\",RESOLUTION=1280x720,AUDIO=\"embedded\"\n\
         muxed.m3u8\n",
    );
    let intent = muxed_intent(1280, 720);
    let mut proofs = ProofQueue::new([muxed_proof()]);
    let snapshot = build_hls_catalog(
        HlsCatalogBuildRequest {
            master: &master,
            catalog_identity: catalog_identity(1, 1),
            provider_default: &intent,
            provider_default_variant_index: None,
            policy: policy(),
        },
        &mut proofs,
    )
    .expect("embedded catalog");

    assert_eq!(snapshot.catalog().coupled_presentations().len(), 1);
    assert!(snapshot.catalog().required_audio_variants().is_err());
}

#[test]
fn audio_group_edge_still_requires_matching_alignment_proof() {
    let master = master(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"English\",LANGUAGE=\"en\",CHANNELS=\"2\",URI=\"en.m3u8\"\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"Commentary\",LANGUAGE=\"en\",CHANNELS=\"2\",URI=\"commentary.m3u8\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.640028,mp4a.40.2\",RESOLUTION=1280x720,AUDIO=\"a\"\n\
         video.m3u8\n",
    );
    let intent = separate_intent(1280, 720, "English");
    let mut proofs = ProofQueue::new([
        video_proof(),
        audio_proof_with_alignment(1),
        audio_proof_with_alignment(2),
    ]);
    let snapshot = build_hls_catalog(
        HlsCatalogBuildRequest {
            master: &master,
            catalog_identity: catalog_identity(1, 1),
            provider_default: &intent,
            provider_default_variant_index: None,
            policy: policy(),
        },
        &mut proofs,
    )
    .expect("only aligned pair is selectable");
    assert_eq!(
        snapshot
            .catalog()
            .compatibility()
            .expect("topology relation")
            .logical_edge_count(),
        1
    );
    assert_eq!(
        snapshot.catalog().required_audio_variants().unwrap().len(),
        2
    );
}

#[test]
fn rejected_sibling_is_isolated_but_rejected_default_is_fatal() {
    let master = master(
        "#EXTM3U\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.640028,mp4a.40.2\",RESOLUTION=1280x720\nselected.m3u8\n\
         #EXT-X-STREAM-INF:BANDWIDTH=2,CODECS=\"avc1.640028,mp4a.40.2\",RESOLUTION=1920x1080\nbroken.m3u8\n",
    );
    let intent = muxed_intent(1280, 720);
    let mut proofs = ProofQueue {
        replies: VecDeque::from([
            Ok(muxed_proof()),
            Err(HlsCatalogChildProofError::Rejected(
                HlsCatalogSiblingRejectionReason::UnsupportedContainer,
            )),
        ]),
        calls: Vec::new(),
    };
    let snapshot = build_hls_catalog(
        HlsCatalogBuildRequest {
            master: &master,
            catalog_identity: catalog_identity(1, 1),
            provider_default: &intent,
            provider_default_variant_index: None,
            policy: policy(),
        },
        &mut proofs,
    )
    .expect("non-default sibling is isolatable");
    assert_eq!(snapshot.catalog().coupled_presentations().len(), 1);
    assert_eq!(snapshot.sibling_rejections().len(), 1);

    let mut rejected_default = ProofQueue {
        replies: VecDeque::from([Err(HlsCatalogChildProofError::Rejected(
            HlsCatalogSiblingRejectionReason::UnsupportedContainer,
        ))]),
        calls: Vec::new(),
    };
    assert!(matches!(
        build_hls_catalog(
            HlsCatalogBuildRequest {
                master: &master,
                catalog_identity: catalog_identity(2, 2),
                provider_default: &intent,
                provider_default_variant_index: None,
                policy: policy(),
            },
            &mut rejected_default,
        ),
        Err(HlsCatalogBuildError::ProviderDefaultRejected {
            reason: HlsCatalogSiblingRejectionReason::UnsupportedContainer
        })
    ));
}

#[test]
fn semantic_selection_survives_uri_query_and_source_order_changes() {
    let first = master(
        "#EXTM3U\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.640028,mp4a.40.2\",RESOLUTION=1280x720\na.m3u8?old=1\n\
         #EXT-X-STREAM-INF:BANDWIDTH=2,CODECS=\"avc1.640028,mp4a.40.2\",RESOLUTION=1920x1080\nb.m3u8?old=1\n",
    );
    let second = master(
        "#EXTM3U\n\
         #EXT-X-STREAM-INF:BANDWIDTH=2,CODECS=\"mp4a.40.2,avc1.640028\",RESOLUTION=1920x1080\nrotated-b.m3u8?new=2\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"mp4a.40.2,avc1.640028\",RESOLUTION=1280x720\nrotated-a.m3u8?new=2\n",
    );
    let intent = muxed_intent(1280, 720);
    let mut first_proofs = ProofQueue::new([muxed_proof(), muxed_proof()]);
    let first_snapshot = build_hls_catalog(
        HlsCatalogBuildRequest {
            master: &first,
            catalog_identity: catalog_identity(1, 1),
            provider_default: &intent,
            provider_default_variant_index: None,
            policy: policy(),
        },
        &mut first_proofs,
    )
    .expect("first snapshot");
    let semantic = first_snapshot
        .provider_default_selection()
        .semantic_rematch_request();

    let mut second_proofs = ProofQueue::new([muxed_proof(), muxed_proof()]);
    let second_snapshot = build_hls_catalog(
        HlsCatalogBuildRequest {
            master: &second,
            catalog_identity: catalog_identity(2, 2),
            provider_default: &intent,
            provider_default_variant_index: None,
            policy: policy(),
        },
        &mut second_proofs,
    )
    .expect("second snapshot");
    second_snapshot
        .rematch_semantic(semantic)
        .expect("semantic row ignores URI, query and source order");
}

#[test]
fn private_reopen_is_exact_initially_and_semantic_without_fallback_after_rotation() {
    let first = master(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"English\",LANGUAGE=\"en\",CHANNELS=\"2\",URI=\"audio.m3u8?old=1\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.640028,mp4a.40.2\",RESOLUTION=1280x720,AUDIO=\"a\"\n\
         video.m3u8?old=1\n",
    );
    let intent = separate_intent(1280, 720, "English");
    let mut proofs = ProofQueue::new([video_proof(), audio_proof()]);
    let snapshot = build_hls_catalog(
        HlsCatalogBuildRequest {
            master: &first,
            catalog_identity: catalog_identity(1, 1),
            provider_default: &intent,
            provider_default_variant_index: None,
            policy: policy(),
        },
        &mut proofs,
    )
    .expect("catalog");
    let reopen = snapshot
        .reopen_exact(snapshot.provider_default_selection())
        .expect("canonical selection");
    let exact = reopen
        .resolve_master(&first, HlsCatalogMatchMode::Exact)
        .expect("original parser rows match exactly");
    assert_eq!(exact.main_reference, first.variants[0].uri);
    assert_eq!(
        exact.audio.expect("selected audio").reference,
        first.renditions[0].uri.clone().expect("audio reference")
    );

    let rotated = master(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"English\",LANGUAGE=\"en\",CHANNELS=\"2\",URI=\"rotated-audio.m3u8?new=2\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"mp4a.40.2,avc1.640028\",RESOLUTION=1280x720,AUDIO=\"a\"\n\
         rotated-video.m3u8?new=2\n",
    );
    assert!(matches!(
        reopen.resolve_master(&rotated, HlsCatalogMatchMode::Exact),
        Err(HlsCatalogReopenError::MissingPrivateRow)
    ));
    let semantic = reopen
        .resolve_master(&rotated, HlsCatalogMatchMode::Semantic)
        .expect("URI and parser order are not semantic identity");
    assert_eq!(semantic.main_reference, rotated.variants[0].uri);
    assert_eq!(
        semantic.audio.expect("rotated audio").reference,
        rotated.renditions[0]
            .uri
            .clone()
            .expect("rotated audio reference")
    );

    let ambiguous = master(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"English\",LANGUAGE=\"en\",CHANNELS=\"2\",URI=\"audio.m3u8\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.640028,mp4a.40.2\",RESOLUTION=1280x720,AUDIO=\"a\"\n\
         first.m3u8\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"mp4a.40.2,avc1.640028\",RESOLUTION=1280x720,AUDIO=\"a\"\n\
         second.m3u8\n",
    );
    assert!(matches!(
        reopen.resolve_master(&ambiguous, HlsCatalogMatchMode::Semantic),
        Err(HlsCatalogReopenError::AmbiguousPrivateRow)
    ));

    let missing_audio = master(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"Spanish\",LANGUAGE=\"es\",CHANNELS=\"2\",URI=\"audio.m3u8\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.640028,mp4a.40.2\",RESOLUTION=1280x720,AUDIO=\"a\"\n\
         video.m3u8\n",
    );
    assert!(matches!(
        reopen.resolve_master(&missing_audio, HlsCatalogMatchMode::Semantic),
        Err(HlsCatalogReopenError::MissingPrivateRow)
    ));
}

#[test]
fn unique_child_is_proven_once_and_budget_fails_before_any_hook() {
    let master = master(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"English\",LANGUAGE=\"en\",CHANNELS=\"2\",URI=\"shared-audio.m3u8?token=one\"\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"Commentary\",LANGUAGE=\"en\",CHANNELS=\"2\",URI=\"shared-audio.m3u8?token=one\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.640028,mp4a.40.2\",RESOLUTION=1280x720,AUDIO=\"a\"\n\
         video.m3u8\n",
    );
    let intent = separate_intent(1280, 720, "English");
    let mut proofs = ProofQueue::new([video_proof(), audio_proof()]);
    let snapshot = build_hls_catalog(
        HlsCatalogBuildRequest {
            master: &master,
            catalog_identity: catalog_identity(1, 1),
            provider_default: &intent,
            provider_default_variant_index: None,
            policy: policy(),
        },
        &mut proofs,
    )
    .expect("shared child catalog");
    assert_eq!(proofs.calls.len(), 2, "variant plus one unique audio child");
    assert_eq!(
        snapshot.catalog().required_audio_variants().unwrap().len(),
        2
    );

    let mut no_proofs = ProofQueue::new([]);
    let error = build_hls_catalog(
        HlsCatalogBuildRequest {
            master: &master,
            catalog_identity: catalog_identity(2, 2),
            provider_default: &intent,
            provider_default_variant_index: None,
            policy: HlsCatalogBuildPolicy {
                maximum_unique_children: NonZeroUsize::new(1).expect("tight child limit"),
                ..policy()
            },
        },
        &mut no_proofs,
    )
    .expect_err("child budget must fail before proof I/O");
    assert!(matches!(
        error,
        HlsCatalogBuildError::UniqueChildLimitExceeded {
            provided: 2,
            maximum: 1
        }
    ));
    assert!(no_proofs.calls.is_empty());
}
