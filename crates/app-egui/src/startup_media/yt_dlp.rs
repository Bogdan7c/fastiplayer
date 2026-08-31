use std::sync::Arc;

/// Подготовленный YtDlp playback source и exact identity выбранного candidate-а.
pub(crate) struct PreparedYtDlpStartupMedia {
    /// Уже открытый neutral S22 demuxer для текущего playback open.
    pub(crate) demuxer: Box<dyn media_core::Demuxer + Send>,

    /// Metadata того же extraction generation не теряется между job и Installed.
    pub(crate) playlist_metadata: service_ytdlp::YtDlpPlaylistMetadata,

    /// Canonical N01 selection и exact lifecycle kind neutral envelope-а.
    pub(crate) neutral_selection: web_media_core::WebMediaSelection,
    pub(crate) presentation: web_media_core::WebMediaPresentationKind,
    pub(crate) extractor_reason: web_media_core::ExtractorInvocationReason,

    /// Exact+semantic token, по которому restore выполняет fresh extraction/rematch.
    pub(crate) candidate_selection: service_ytdlp::YtDlpCandidateSelection,
    pub(crate) composed_selection: Option<Box<service_ytdlp::YtDlpComposedSelection>>,

    /// UI-safe inventory того же extraction snapshot-а.
    pub(crate) stream_configuration: crate::web_media_stream_model::WebMediaStreamConfiguration,

    /// Runtime-only declared yt-dlp catalog attachment.
    pub(crate) catalog_attachment: crate::web_media_catalog::WebMediaCatalogAttachment,

    /// S31L publication boundary для HLS live; VOD оставляет поле пустым.
    pub(crate) timeline_port: Option<media_core::DynamicMediaTimelinePort>,

    /// Worker-receipted static DASH/Smooth/HDS seek boundary.
    pub(crate) demux_seek_port: Option<Arc<dyn player_core::PreparedDemuxSeekPort>>,

    /// Optional absolute source window для zero-based HDS presentation.
    pub(crate) playback_window: Option<player_core::MediaPlaybackWindow>,

    /// VOD-only transport gate должен пережить background startup preparation.
    pub(crate) vod_endpoint_recovery:
        Option<crate::web_media_vod_recovery::VodEndpointRecoveryAttachment>,
}
