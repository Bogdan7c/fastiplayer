use crate::media_open::{
    PreparedWebMediaAttachments, PreparedWebMediaEnvelope, SafeMediaLabel, WebMediaSourceIntent,
    compose_prepared_web_media,
};

use super::PreparedStartupMedia;

/// Полностью собранный native-HLS startup envelope до strong-install barrier-а.
pub(super) struct ComposedNativeHlsStartupMedia {
    /// Player-owned media с authoritative post-target seek attachment.
    pub(super) prepared_media: player_core::PreparedMedia,
    /// Durable provider-neutral source intent для lifecycle/reopen.
    pub(super) active_source: crate::media_open::ActiveMediaSource,
    /// Safe label передаётся UI без physical manifest locator-а.
    pub(super) safe_label: SafeMediaLabel,
    /// Immutable descriptor переносит tracks/metadata к Installed state.
    pub(super) descriptor: PreparedWebMediaEnvelope,
}

/// Полностью собранный native-DASH startup envelope до strong-install barrier-а.
pub(super) struct ComposedNativeDashStartupMedia {
    /// Player-owned media с worker-receipted VOD seek attachment.
    pub(super) prepared_media: player_core::PreparedMedia,
    /// Durable provider-neutral source intent для lifecycle/reopen.
    pub(super) active_source: crate::media_open::ActiveMediaSource,
    /// Safe label передаётся UI без physical MPD locator-а.
    pub(super) safe_label: SafeMediaLabel,
    /// Immutable descriptor переносит tracks/catalog/recovery к Installed state.
    pub(super) descriptor: PreparedWebMediaEnvelope,
}

/// Полностью собранный native-Smooth startup envelope до strong-install barrier-а.
pub(super) struct ComposedNativeSmoothStartupMedia {
    /// Player-owned media с worker-receipted VOD seek attachment.
    pub(super) prepared_media: player_core::PreparedMedia,
    /// Durable provider-neutral source intent для lifecycle/reopen.
    pub(super) active_source: crate::media_open::ActiveMediaSource,
    /// Safe label передаётся UI без physical `/Manifest` locator-а.
    pub(super) safe_label: SafeMediaLabel,
    /// Immutable descriptor переносит tracks/catalog/recovery к Installed state.
    pub(super) descriptor: PreparedWebMediaEnvelope,
}

/// Собирает direct startup result через тот же neutral web envelope, что обычный media-open.
pub(super) fn compose_direct_startup_media(
    source_locator: service_direct_media::DirectMediaUrl,
    opened_media: crate::direct_progressive_open::DirectProgressiveOpenResult,
) -> PreparedStartupMedia {
    let source_label = opened_media.source_label().to_owned();
    let tracks = opened_media.tracks().to_vec();
    let duration = opened_media.duration();
    let metadata = opened_media.media_metadata().unwrap_or_default().tags;
    let safe_label = SafeMediaLabel::from_service_safe_label(&source_label);
    let source = WebMediaSourceIntent::direct(source_locator.clone());
    let descriptor =
        PreparedWebMediaEnvelope::new(tracks, duration, metadata, source, safe_label, None, None);
    let prepared_media = compose_prepared_web_media(
        &source_label,
        opened_media.into_demuxer(),
        PreparedWebMediaAttachments::default(),
    )
    .expect("direct VOD has no conflicting timeline attachments");

    PreparedStartupMedia::Direct {
        source_locator,
        prepared_media,
        descriptor: Box::new(descriptor),
    }
}

/// Собирает native HLS startup result через общий web composition boundary.
pub(super) fn compose_native_hls_startup_media(
    source: crate::media_open::NativeHlsUrl,
    prepared: crate::startup_media::native_hls::PreparedNativeHlsMedia,
) -> Result<ComposedNativeHlsStartupMedia, String> {
    let tracks = prepared.demuxer.tracks().to_vec();
    let duration = prepared.demuxer.duration();
    let metadata = prepared.demuxer.media_metadata().unwrap_or_default().tags;
    let safe_label = source.safe_label().clone();
    let runtime_attachments = prepared.lifecycle.into_web_attachments(prepared.seek_port);
    let source_intent = WebMediaSourceIntent::native_hls(
        source,
        runtime_attachments.presentation,
        prepared.source_state,
    );
    let active_source = crate::media_open::ActiveMediaSource::Web(source_intent.clone());
    let prepared_media = compose_prepared_web_media(
        safe_label.as_str(),
        prepared.demuxer,
        runtime_attachments.prepared,
    )
    .map_err(|error| error.to_string())?;
    let descriptor = PreparedWebMediaEnvelope::new(
        tracks,
        duration,
        metadata,
        source_intent,
        safe_label.clone(),
        None,
        runtime_attachments.vod_endpoint_recovery,
    );

    Ok(ComposedNativeHlsStartupMedia {
        prepared_media,
        active_source,
        safe_label,
        descriptor,
    })
}

/// Собирает native static DASH startup result через общий web composition boundary.
pub(super) fn compose_native_dash_startup_media(
    source: crate::media_open::NativeDashUrl,
    prepared: crate::startup_media::native_dash::PreparedNativeDashMedia,
) -> Result<ComposedNativeDashStartupMedia, String> {
    let crate::startup_media::native_dash::PreparedNativeDashMedia {
        demuxer,
        seek_port,
        source_state,
        lifecycle,
    } = prepared;
    let tracks = demuxer.tracks().to_vec();
    let duration = demuxer.duration();
    let metadata = demuxer.media_metadata().unwrap_or_default().tags;
    let safe_label = source.safe_label().clone();
    let runtime_attachments = lifecycle.into_web_attachments(seek_port);
    let source_intent =
        WebMediaSourceIntent::native_dash(source, runtime_attachments.presentation, source_state);
    let active_source = crate::media_open::ActiveMediaSource::Web(source_intent.clone());
    let prepared_media =
        compose_prepared_web_media(safe_label.as_str(), demuxer, runtime_attachments.prepared)
            .map_err(|error| error.to_string())?;
    let descriptor = PreparedWebMediaEnvelope::new(
        tracks,
        duration,
        metadata,
        source_intent,
        safe_label.clone(),
        None,
        runtime_attachments.vod_endpoint_recovery,
    );
    Ok(ComposedNativeDashStartupMedia {
        prepared_media,
        active_source,
        safe_label,
        descriptor,
    })
}

/// Собирает native Smooth startup result через общий web composition boundary.
pub(super) fn compose_native_smooth_startup_media(
    source: crate::media_open::NativeSmoothUrl,
    prepared: crate::startup_media::native_smooth::PreparedNativeSmoothMedia,
) -> Result<ComposedNativeSmoothStartupMedia, String> {
    let crate::startup_media::native_smooth::PreparedNativeSmoothMedia {
        demuxer,
        seek_port,
        source_state,
        endpoint_recovery,
    } = prepared;
    let tracks = demuxer.tracks().to_vec();
    let duration = demuxer.duration();
    let metadata = demuxer.media_metadata().unwrap_or_default().tags;
    let safe_label = source.safe_label().clone();
    let source_intent = WebMediaSourceIntent::native_smooth(source, source_state);
    let active_source = crate::media_open::ActiveMediaSource::Web(source_intent.clone());
    let prepared_media = compose_prepared_web_media(
        safe_label.as_str(),
        demuxer,
        PreparedWebMediaAttachments {
            demux_seek: Some(
                crate::media_open::PreparedWebMediaSeekAttachment::WorkerReceipted(seek_port),
            ),
            ..PreparedWebMediaAttachments::default()
        },
    )
    .map_err(|error| error.to_string())?;
    let descriptor = PreparedWebMediaEnvelope::new(
        tracks,
        duration,
        metadata,
        source_intent,
        safe_label.clone(),
        None,
        Some(endpoint_recovery),
    );
    Ok(ComposedNativeSmoothStartupMedia {
        prepared_media,
        active_source,
        safe_label,
        descriptor,
    })
}
