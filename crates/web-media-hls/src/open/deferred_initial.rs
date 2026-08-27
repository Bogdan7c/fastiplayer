//! Deferred component open transaction и initial-position proof publication.

use anyhow::Result;
use media_core::{DemuxSeekResult, Demuxer};

use super::validate_track_shape;
use crate::epoch_demux::{
    HlsComponentFactory, HlsInitialComponentOpen, HlsInitialPositionEvidence,
};
use crate::initial_position_proof::HlsInitialPositionProofPublisher;
use crate::transactional_av::TransactionalHlsAvDemuxer;
use crate::{HlsMainTrackLayoutIntent, HlsVodOpenPolicy};

/// Полностью собранный component recipe перемещается в один deferred worker closure.
pub(super) struct HlsDeferredInitialComponent {
    pub(super) factory: HlsComponentFactory,
    pub(super) initial_open: HlsInitialComponentOpen,
}

/// Открывает main/alternate audio атомарно и только затем публикует initial proof.
pub(super) fn open_deferred_initial_components(
    main: HlsDeferredInitialComponent,
    audio: Option<HlsDeferredInitialComponent>,
    main_track_layout: HlsMainTrackLayoutIntent,
    policy: HlsVodOpenPolicy,
    proof_publisher: HlsInitialPositionProofPublisher,
) -> Result<Box<dyn Demuxer + Send>> {
    let opened = open_components(main, audio, main_track_layout, policy);
    match opened {
        Ok((demuxer, evidence)) => {
            let publication = match evidence {
                HlsInitialPositionEvidence::Beginning => proof_publisher.publish_beginning(),
                HlsInitialPositionEvidence::Positioned(result) => proof_publisher.publish(result),
            };
            if let Err(error) = publication {
                proof_publisher.publish_failure();
                return Err(error.into());
            }
            Ok(demuxer)
        }
        Err(error) => {
            proof_publisher.publish_failure();
            Err(error)
        }
    }
}

fn open_components(
    main: HlsDeferredInitialComponent,
    audio: Option<HlsDeferredInitialComponent>,
    main_track_layout: HlsMainTrackLayoutIntent,
    policy: HlsVodOpenPolicy,
) -> Result<(Box<dyn Demuxer + Send>, HlsInitialPositionEvidence)> {
    let mut main_demuxer = main.factory.open_initial(main.initial_open)?;
    let main_evidence = main_demuxer.take_initial_position_evidence();
    let Some(audio) = audio else {
        validate_track_shape(main_demuxer.tracks(), main_track_layout, "main")?;
        main_demuxer.activate_committed_read()?;
        return Ok((Box::new(main_demuxer), main_evidence));
    };

    let mut audio_demuxer = audio.factory.open_initial(audio.initial_open)?;
    let audio_evidence = audio_demuxer.take_initial_position_evidence();
    validate_track_shape(
        main_demuxer.tracks(),
        HlsMainTrackLayoutIntent::VideoOnly,
        "alternate-video main",
    )?;
    validate_track_shape(
        audio_demuxer.tracks(),
        HlsMainTrackLayoutIntent::AudioOnly,
        "alternate-audio",
    )?;
    let combined_evidence = combine_component_evidence(main_evidence, audio_evidence)?;
    let composite = TransactionalHlsAvDemuxer::new(
        main.factory,
        audio.factory,
        main_demuxer,
        audio_demuxer,
        policy.composite_lead_policy,
    )?;
    Ok((Box::new(composite), combined_evidence))
}

/// Video landing остаётся public result, но alternate audio обязан доказать тот же target.
fn combine_component_evidence(
    main: HlsInitialPositionEvidence,
    audio: HlsInitialPositionEvidence,
) -> Result<HlsInitialPositionEvidence> {
    match (main, audio) {
        (HlsInitialPositionEvidence::Beginning, HlsInitialPositionEvidence::Beginning) => {
            Ok(HlsInitialPositionEvidence::Beginning)
        }
        (
            HlsInitialPositionEvidence::Positioned(main_result),
            HlsInitialPositionEvidence::Positioned(audio_result),
        ) => {
            validate_separate_audio_result(main_result, audio_result)?;
            Ok(HlsInitialPositionEvidence::Positioned(main_result))
        }
        _ => anyhow::bail!("HLS initial main/audio position evidence lifecycle mismatch"),
    }
}

fn validate_separate_audio_result(
    main_result: DemuxSeekResult,
    audio_result: DemuxSeekResult,
) -> Result<()> {
    if main_result.requested_position != audio_result.requested_position {
        anyhow::bail!("HLS initial main/audio proof requested targets differ");
    }
    let latest_covered_audio_landing =
        if main_result.actual_position >= main_result.requested_position {
            main_result.actual_position
        } else {
            main_result.requested_position
        };
    if audio_result.actual_position > latest_covered_audio_landing {
        anyhow::bail!("HLS initial audio landing starts after authoritative video coverage");
    }
    Ok(())
}
