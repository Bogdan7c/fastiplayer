use web_media_core::{
    CodecFamily, DynamicRange, FrameRate, NormalizedCodec, RawCodecIdentity, VideoHeight,
    VideoTrackDescriptor, VideoWidth,
};

use super::model::{WebMediaFacet, WebMediaVerifiedCatalog};
use super::*;

#[test]
fn dependent_facets_keep_one_item_and_automatic_missing_metadata_visible() {
    let target = WebMediaSelectionTarget::Fixture(1);
    let catalog = WebMediaVerifiedCatalog::new(
        7,
        crate::web_media_stream_model::WebMediaStreamGeneration::for_test(1, 1),
        vec![WebMediaCatalogChoice {
            mode: WebMediaMode::VideoAndAudio,
            video: Some(VideoTrackDescriptor::new(
                NormalizedCodec::parse(RawCodecIdentity::new("avc1.640028").unwrap()),
                None,
                Some(VideoHeight::new(1080).unwrap()),
                None,
                None,
                DynamicRange::Unknown,
            )),
            rank: web_media_playback_plan::OpaqueAlternativeRank::parent(0),
            target: target.clone(),
        }],
        &target,
        2,
        3,
    )
    .unwrap();

    let projection = catalog.picker_projection();
    assert_eq!(projection.rejected_siblings, 2);
    assert_eq!(projection.unprobed_siblings, 3);
    assert!(
        projection
            .selectors
            .iter()
            .all(|selector| !selector.options.is_empty())
    );
    assert!(
        projection
            .selectors
            .iter()
            .any(|selector| { selector.options.as_ref() == [WebMediaFacetOption::Automatic] })
    );
}

#[test]
fn upper_facet_change_preserves_reachable_lower_facets() {
    let h264 = NormalizedCodec::parse(RawCodecIdentity::new("avc1.640028").unwrap());
    let vp9 = NormalizedCodec::parse(RawCodecIdentity::new("vp09.00.10.08").unwrap());
    let choices = [h264, vp9]
        .into_iter()
        .enumerate()
        .map(|(index, codec)| WebMediaCatalogChoice {
            mode: WebMediaMode::VideoAndAudio,
            video: Some(VideoTrackDescriptor::new(
                codec,
                None,
                Some(VideoHeight::new(1080).unwrap()),
                None,
                None,
                DynamicRange::Sdr,
            )),
            rank: web_media_playback_plan::OpaqueAlternativeRank::parent(index),
            target: WebMediaSelectionTarget::Fixture(index as u64),
        })
        .collect::<Vec<_>>();
    let active = choices[0].target.clone();
    let catalog = WebMediaVerifiedCatalog::new(
        9,
        crate::web_media_stream_model::WebMediaStreamGeneration::for_test(1, 1),
        choices,
        &active,
        0,
        0,
    )
    .unwrap();
    let codec = catalog
        .picker_projection()
        .selectors
        .iter()
        .find(|selector| selector.facet == WebMediaFacet::Codec)
        .unwrap()
        .clone();
    let action = WebMediaFacetAction {
        generation: 9,
        facet: WebMediaFacet::Codec,
        option_index: usize::from(codec.selected_index == Some(0)),
    };
    let selected = catalog.resolve_facet_action(action).unwrap();
    assert_eq!(selected, &WebMediaSelectionTarget::Fixture(1));
}

#[test]
fn lower_facet_action_never_escapes_selected_upper_prefix() {
    let h264 = NormalizedCodec::parse(RawCodecIdentity::new("avc1.640028").unwrap());
    let vp9 = NormalizedCodec::parse(RawCodecIdentity::new("vp09.00.10.08").unwrap());
    let choice = |target, codec, height, frame_rate, dynamic_range| WebMediaCatalogChoice {
        mode: WebMediaMode::VideoAndAudio,
        video: Some(VideoTrackDescriptor::new(
            codec,
            Some(VideoWidth::new(height * 16 / 9).unwrap()),
            Some(VideoHeight::new(height).unwrap()),
            Some(FrameRate::new(frame_rate, 1).unwrap()),
            None,
            dynamic_range,
        )),
        rank: web_media_playback_plan::OpaqueAlternativeRank::parent(target as usize),
        target: WebMediaSelectionTarget::Fixture(target),
    };
    let choices = vec![
        choice(0, h264.clone(), 720, 30, DynamicRange::Sdr),
        choice(1, h264, 1080, 60, DynamicRange::Hdr),
        choice(2, vp9, 1080, 30, DynamicRange::Sdr),
    ];
    let active = choices[0].target.clone();
    let catalog = WebMediaVerifiedCatalog::new(
        11,
        crate::web_media_stream_model::WebMediaStreamGeneration::for_test(1, 1),
        choices,
        &active,
        0,
        0,
    )
    .unwrap();
    let projection = catalog.picker_projection();
    let resolution = projection
        .selectors
        .iter()
        .find(|selector| selector.facet == WebMediaFacet::Resolution)
        .unwrap();
    let option_index = resolution
        .options
        .iter()
        .position(|option| {
            *option
                == WebMediaFacetOption::Resolution {
                    width: 1920,
                    height: 1080,
                }
        })
        .unwrap();

    assert_eq!(
        catalog.resolve_facet_action(WebMediaFacetAction {
            generation: 11,
            facet: WebMediaFacet::Resolution,
            option_index,
        }),
        Some(&WebMediaSelectionTarget::Fixture(1))
    );
    assert!(
        catalog
            .resolve_facet_action(WebMediaFacetAction {
                generation: 10,
                facet: WebMediaFacet::Resolution,
                option_index,
            })
            .is_none()
    );
}

#[test]
fn identical_visible_alternatives_use_planner_rank_not_catalog_order() {
    let h264 = NormalizedCodec::parse(RawCodecIdentity::new("avc1.640028").unwrap());
    let vp9 = NormalizedCodec::parse(RawCodecIdentity::new("vp09.00.10.08").unwrap());
    let choice = |target, codec, rank| WebMediaCatalogChoice {
        mode: WebMediaMode::VideoAndAudio,
        video: Some(VideoTrackDescriptor::new(
            codec,
            Some(VideoWidth::new(1920).unwrap()),
            Some(VideoHeight::new(1080).unwrap()),
            Some(FrameRate::new(30, 1).unwrap()),
            None,
            DynamicRange::Sdr,
        )),
        rank: web_media_playback_plan::OpaqueAlternativeRank::parent(rank),
        target: WebMediaSelectionTarget::Fixture(target),
    };
    let active = choice(0, h264, 2);
    let preferred = choice(1, vp9.clone(), 0);
    let fallback = choice(2, vp9, 1);

    for choices in [
        vec![active.clone(), preferred.clone(), fallback.clone()],
        vec![fallback.clone(), preferred.clone(), active.clone()],
    ] {
        let catalog = WebMediaVerifiedCatalog::new(
            12,
            crate::web_media_stream_model::WebMediaStreamGeneration::for_test(1, 1),
            choices,
            &active.target,
            0,
            0,
        )
        .unwrap();
        let projection = catalog.picker_projection();
        let codec = projection
            .selectors
            .iter()
            .find(|selector| selector.facet == WebMediaFacet::Codec)
            .unwrap();
        let option_index = codec
            .options
            .iter()
            .position(|option| *option == WebMediaFacetOption::Codec(CodecFamily::Vp9))
            .unwrap();

        assert_eq!(
            catalog.resolve_facet_action(WebMediaFacetAction {
                generation: 12,
                facet: WebMediaFacet::Codec,
                option_index,
            }),
            Some(&preferred.target)
        );
    }
}
