//! Сборка native HDS source в общий prepared-media envelope.
//!
//! Модуль удерживает HDS-specific admission, typed fallback и runtime attachments
//! за отдельной границей, чтобы центральный dispatcher не знал устройство HDS runtime.

use super::{
    MediaOpenSourceRequest, MediaPreparationFailureKind, NativeHdsOpenIntent, NativeHdsUrl,
    PreparedMediaDescriptor, PreparedMediaOpen, PreparedWebMediaAttachments,
    PreparedWebMediaEnvelope, PreparedWebMediaSeekAttachment, WebMediaOpenRequest,
    WebMediaOpenSettings, WebMediaSourceIntent, compose_prepared_web_media,
};

/// Подготавливает native HDS source либо единожды передаёт initial admission extractor-у.
pub(super) fn prepare_native_hds_source(
    source: NativeHdsUrl,
    intent: NativeHdsOpenIntent,
    settings: WebMediaOpenSettings,
    cancellation: &super::executor::PreparationCancellation,
) -> Result<PreparedMediaOpen, MediaPreparationFailureKind> {
    let WebMediaOpenSettings {
        network_config,
        web_media_config,
        yt_dlp_config,
        demux_config,
        preferred_video_codec_order,
        system_capabilities,
        audio_capabilities,
    } = settings;
    let safe_label = source.safe_label().clone();
    let (expected_selection, fallback_locator) = match intent {
        NativeHdsOpenIntent::InitialWithYtDlpFallback { fallback_locator } => {
            (None, Some(fallback_locator))
        }
        NativeHdsOpenIntent::SemanticSelection(selection) => (Some(selection), None),
    };
    let attempt = crate::startup_media::native_hds::prepare_native_hds_attempt(
        crate::startup_media::native_hds::NativeHdsPreparationRequest {
            source: &source,
            expected_selection: expected_selection.as_ref(),
            network_config: &network_config,
            web_media_config: &web_media_config,
            demux_config: &demux_config,
            system_capabilities: &system_capabilities,
            audio_capabilities,
            cancellation: cancellation.source_token(),
        },
    )
    .map_err(|error| {
        tracing::warn!(
            source = %safe_label,
            error = %error,
            failure_kind = ?crate::startup_media::native_hds::native_hds_failure_kind(&error),
            "Подготовка native HDS завершилась ошибкой"
        );
        if cancellation.is_cancelled() {
            MediaPreparationFailureKind::Cancelled
        } else {
            MediaPreparationFailureKind::NativeHdsOpen
        }
    })?;

    match attempt {
        crate::startup_media::native_hds::NativeHdsAttempt::Prepared(prepared) => {
            if cancellation.is_cancelled() {
                return Err(MediaPreparationFailureKind::Cancelled);
            }
            install_prepared_native_hds(source, safe_label, prepared)
        }
        crate::startup_media::native_hds::NativeHdsAttempt::RequiresYtDlpFallback(reason) => {
            let Some(locator) = fallback_locator else {
                tracing::warn!(
                    source = %safe_label,
                    ?reason,
                    "Exact native HDS reopen отклонён без extractor fallback"
                );
                return Err(MediaPreparationFailureKind::NativeHdsOpen);
            };
            if !yt_dlp_config.enabled {
                tracing::warn!(
                    source = %safe_label,
                    ?reason,
                    "Initial native HDS fallback запрещён отключённым extractor-ом"
                );
                return Err(MediaPreparationFailureKind::NativeHdsOpen);
            }
            tracing::info!(
                source = %safe_label,
                ?reason,
                "Initial native HDS admission передан единственному YtDlp fallback"
            );
            super::preparation::prepare_source(
                MediaOpenSourceRequest::Web(WebMediaOpenRequest::extractor(
                    locator,
                    crate::web_media_open::YtDlpCandidateOpenIntent::BestPlayable,
                    WebMediaOpenSettings {
                        network_config,
                        web_media_config,
                        yt_dlp_config,
                        demux_config,
                        preferred_video_codec_order,
                        system_capabilities,
                        audio_capabilities,
                    },
                )),
                cancellation,
            )
        }
    }
}

/// Устанавливает HDS demux/seek/window как одну непротиворечивую prepared сущность.
fn install_prepared_native_hds(
    source: NativeHdsUrl,
    safe_label: super::SafeMediaLabel,
    prepared: crate::startup_media::native_hds::PreparedNativeHdsMedia,
) -> Result<PreparedMediaOpen, MediaPreparationFailureKind> {
    let crate::startup_media::native_hds::PreparedNativeHdsMedia {
        demuxer,
        seek_port,
        playback_window,
        source_state,
        endpoint_recovery,
    } = prepared;
    let tracks = demuxer.tracks().to_vec();
    let duration = playback_window.end_exclusive().and_then(|end| {
        end.as_duration()
            .checked_sub(playback_window.start().as_duration())
    });
    let metadata = demuxer.media_metadata().unwrap_or_default().tags;
    let active_source = WebMediaSourceIntent::native_hds(source, source_state);
    let prepared_media = compose_prepared_web_media(
        safe_label.as_str(),
        demuxer,
        PreparedWebMediaAttachments {
            demux_seek: Some(PreparedWebMediaSeekAttachment::WorkerReceipted(seek_port)),
            playback_window: Some(playback_window),
            ..PreparedWebMediaAttachments::default()
        },
    )
    .map_err(|error| {
        tracing::warn!(
            source = %safe_label,
            error = %error,
            "Native HDS composition нарушила prepared attachment contract"
        );
        MediaPreparationFailureKind::NativeHdsOpen
    })?;

    Ok(PreparedMediaOpen {
        prepared_media,
        descriptor: PreparedMediaDescriptor::Web(PreparedWebMediaEnvelope::new(
            tracks,
            duration,
            metadata,
            active_source,
            safe_label,
            Some(playback_window),
            Some(endpoint_recovery),
        )),
    })
}
