//! Классификация отказа player-а: каждый вид ошибки даёт ожидаемую причину.

use player_core::{MediaInstallFailure, MediaInstallFailureStage, PlayerError, PlayerErrorKind};

use super::PlayerInstallFailureReason;
use crate::media_open::PlayerDispatchRejection;

/// Отказ player-а на обычной стадии подготовки видео с заданным видом ошибки.
fn video_configuration_failure(kind: PlayerErrorKind) -> MediaInstallFailure {
    MediaInstallFailure::new(
        MediaInstallFailureStage::VideoStreamConfiguration,
        PlayerError::new(kind, "технические детали для лога"),
    )
}

#[test]
fn every_unsupported_video_property_is_reported_as_unsupported_video_format() {
    for kind in [
        PlayerErrorKind::UnsupportedVideoCodec,
        PlayerErrorKind::UnsupportedVideoProfile,
        PlayerErrorKind::UnsupportedVideoBitDepth,
        PlayerErrorKind::UnsupportedVideoChroma,
        PlayerErrorKind::UnsupportedHdrMode,
    ] {
        assert_eq!(
            PlayerInstallFailureReason::from_install_failure(&video_configuration_failure(
                kind.clone()
            )),
            PlayerInstallFailureReason::UnsupportedVideoFormat,
            "{kind:?}"
        );
    }
}

#[test]
fn unsupported_audio_codec_is_reported_as_unsupported_audio_format() {
    let failure = MediaInstallFailure::new(
        MediaInstallFailureStage::AudioTrackPlanning,
        PlayerError::new(PlayerErrorKind::UnsupportedAudioCodec, "Audio error"),
    );

    assert_eq!(
        PlayerInstallFailureReason::from_install_failure(&failure),
        PlayerInstallFailureReason::UnsupportedAudioFormat
    );
}

#[test]
fn missing_hardware_or_required_backend_is_reported_as_no_suitable_decoder() {
    for kind in [
        PlayerErrorKind::HardwareDecoderUnavailable,
        PlayerErrorKind::RequiredVideoBackendUnavailable,
    ] {
        let failure = MediaInstallFailure::new(
            MediaInstallFailureStage::CandidateVideoResourceAcquisition,
            PlayerError::new(kind.clone(), "backend недоступен"),
        );
        assert_eq!(
            PlayerInstallFailureReason::from_install_failure(&failure),
            PlayerInstallFailureReason::NoSuitableVideoDecoder,
            "{kind:?}"
        );
    }
}

/// Player оформляет таймаут подготовки как `DemuxError`; стадия должна победить вид.
#[test]
fn preflight_timeout_stage_wins_over_demux_error_kind() {
    let timeout = MediaInstallFailure::new(
        MediaInstallFailureStage::VideoPreflightTimeout,
        PlayerError::new(PlayerErrorKind::DemuxError, "превысил deadline"),
    );
    let ordinary_demux_error = video_configuration_failure(PlayerErrorKind::DemuxError);

    assert_eq!(
        PlayerInstallFailureReason::from_install_failure(&timeout),
        PlayerInstallFailureReason::PreparationTimedOut
    );
    assert_eq!(
        PlayerInstallFailureReason::from_install_failure(&ordinary_demux_error),
        PlayerInstallFailureReason::InternalError
    );
}

#[test]
fn protocol_and_runtime_kinds_are_internal_errors() {
    for kind in [
        PlayerErrorKind::UnsupportedRenderFormat,
        PlayerErrorKind::DemuxError,
        PlayerErrorKind::SeekUnavailable,
        PlayerErrorKind::SeekTimeout,
        PlayerErrorKind::SeekTargetExpired,
        PlayerErrorKind::DecoderFlushFailed,
        PlayerErrorKind::NetworkError,
        PlayerErrorKind::AudioDeviceUnavailable,
        PlayerErrorKind::RenderDeviceLost,
        PlayerErrorKind::ConfigError,
        PlayerErrorKind::RuntimeError,
        PlayerErrorKind::InvalidCommand,
    ] {
        assert_eq!(
            PlayerInstallFailureReason::from_install_failure(&video_configuration_failure(
                kind.clone()
            )),
            PlayerInstallFailureReason::InternalError,
            "{kind:?}"
        );
    }
}

/// Классификация по виду ошибки не зависит от стадии (кроме таймаута подготовки).
#[test]
fn kind_classification_is_stable_across_non_timeout_stages() {
    for stage in MediaInstallFailureStage::ALL {
        if stage == MediaInstallFailureStage::VideoPreflightTimeout {
            continue;
        }
        let failure = MediaInstallFailure::new(
            stage,
            PlayerError::new(PlayerErrorKind::UnsupportedVideoCodec, "codec"),
        );
        assert_eq!(
            PlayerInstallFailureReason::from_install_failure(&failure),
            PlayerInstallFailureReason::UnsupportedVideoFormat,
            "{stage:?}"
        );
    }
}

#[test]
fn undelivered_install_command_means_player_not_responding() {
    for rejection in [
        PlayerDispatchRejection::Backpressure,
        PlayerDispatchRejection::Disconnected,
    ] {
        assert_eq!(
            PlayerInstallFailureReason::from_dispatch_rejection(rejection),
            PlayerInstallFailureReason::PlayerNotResponding,
            "{rejection:?}"
        );
    }
}
