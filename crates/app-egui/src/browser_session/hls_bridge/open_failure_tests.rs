//! Классы отказа открытия HLS-поколения.

use anyhow::Context;
use browser_bridge_protocol::session::player::StreamRejection;
use demux_api::DemuxOpenError;
use source_core::{HttpRequestPolicyFailure, SourceError};
use web_media_core::WebMediaFallbackTrigger;

use super::{rejection_for_fallback_trigger, rejection_for_preparation_error};

#[test]
fn every_fallback_trigger_becomes_an_honest_rejection() {
    let cases = [
        (
            WebMediaFallbackTrigger::ProviderDocument,
            StreamRejection::UnparsableMedia,
        ),
        (
            WebMediaFallbackTrigger::MalformedManifest,
            StreamRejection::UnparsableMedia,
        ),
        (
            WebMediaFallbackTrigger::UnsupportedNativeProfile,
            StreamRejection::UnsupportedCodec,
        ),
        (
            WebMediaFallbackTrigger::ExtractorOwnedAuthorizationMaterial,
            StreamRejection::FileUnavailable,
        ),
        (
            WebMediaFallbackTrigger::ExpiredEndpoint,
            StreamRejection::FileUnavailable,
        ),
        (
            WebMediaFallbackTrigger::NetworkFailure,
            StreamRejection::FileUnavailable,
        ),
        (
            WebMediaFallbackTrigger::Backpressure,
            StreamRejection::FileUnavailable,
        ),
        (
            WebMediaFallbackTrigger::Cancellation,
            StreamRejection::FileUnavailable,
        ),
        (
            WebMediaFallbackTrigger::DecoderFailure,
            StreamRejection::UnsupportedCodec,
        ),
        (
            WebMediaFallbackTrigger::RendererFailure,
            StreamRejection::PlaybackFailed,
        ),
        (
            WebMediaFallbackTrigger::InvariantViolation,
            StreamRejection::PlaybackFailed,
        ),
    ];
    for (trigger, expected) in cases {
        assert_eq!(
            rejection_for_fallback_trigger(trigger).reason,
            expected,
            "{trigger:?}"
        );
    }
}

/// Ресурс, не полученный от страницы (отказ фона, адрес не номерок), — «не загрузилось»,
/// хотя общая классификация назвала бы его «некорректным ответом сервера».
#[test]
fn a_resource_the_page_did_not_give_is_file_unavailable() {
    for reason in [
        HttpRequestPolicyFailure::ResourcePolicyRejected,
        HttpRequestPolicyFailure::TargetResolution,
    ] {
        let error = anyhow::Error::new(SourceError::HttpRequestPolicyRejected { reason })
            .context("HLS root fetch")
            .context("native HLS preparation");
        let rejection = rejection_for_preparation_error(&error);
        assert_eq!(
            rejection.reason,
            StreamRejection::FileUnavailable,
            "{reason:?}"
        );
        assert_eq!(rejection.class, "resource_unavailable");
    }
}

/// Байты пришли, но не разобрались — «нечитаемо».
#[test]
fn bytes_that_do_not_demux_are_unparsable() {
    let error = Err::<(), _>(DemuxOpenError::NoMatch)
        .context("HLS main content probe")
        .expect_err("error");
    assert_eq!(
        rejection_for_preparation_error(&error).reason,
        StreamRejection::UnparsableMedia
    );
}

/// Ошибка без типизированных звеньев — не класс страницы: «не смогли сыграть».
#[test]
fn an_untyped_error_is_a_playback_failure() {
    let error = anyhow::anyhow!("something inside the player broke");
    let rejection = rejection_for_preparation_error(&error);
    assert_eq!(rejection.reason, StreamRejection::PlaybackFailed);
    assert_eq!(rejection.class, "unclassified");
}
