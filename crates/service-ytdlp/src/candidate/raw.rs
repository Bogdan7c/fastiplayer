use serde::Deserialize;
use serde_json::Value;

/// Raw result одного `--dump-single-json` extraction snapshot-а.
///
/// `selected_format` читает только format-level поля корневого result-а.
/// `formats` остаётся единственным inventory, а `requested_formats` никогда не
/// подменяет inventory и рассматривается только при compound reconstruction.
#[derive(Deserialize)]
pub(crate) struct YtDlpCandidateDocument {
    /// Public extractor inventory.
    pub(crate) formats: Option<Vec<YtDlpSerializedFormat>>,
    /// Pinned selected compound components.
    pub(crate) requested_formats: Option<Vec<YtDlpSerializedFormat>>,
    /// Selected ordinary result, представленный format fields корневого JSON.
    #[serde(flatten)]
    pub(crate) selected_format: YtDlpSerializedFormat,
}

/// Public serialized yt-dlp format fields, нужные S19 normalization boundary.
///
/// Request-material shapes с неоднородной upstream JSON-формой сначала
/// сохраняются как `Value`: semantic validation выполняется отдельно и
/// превращает проблему одной строки в visible rejection, а не теряет весь
/// inventory из-за ошибки deserialization.
#[derive(Clone, Default, Deserialize)]
pub(crate) struct YtDlpSerializedFormat {
    /// Snapshot-local format identity.
    pub(crate) format_id: Option<String>,
    /// Effective request endpoint.
    pub(crate) url: Option<String>,
    /// Upstream manifest endpoint.
    pub(crate) manifest_url: Option<String>,
    /// Raw transport identity.
    pub(crate) protocol: Option<String>,
    /// File/container extension hint.
    pub(crate) ext: Option<String>,
    /// Более точный container hint.
    pub(crate) container: Option<String>,
    /// Raw video codec identity либо explicit `none`.
    pub(crate) vcodec: Option<String>,
    /// Raw audio codec identity либо explicit `none`.
    pub(crate) acodec: Option<String>,
    /// Video width.
    pub(crate) width: Option<u32>,
    /// Video height.
    pub(crate) height: Option<u32>,
    /// Frame rate от JSON number.
    pub(crate) fps: Option<f64>,
    /// Total bitrate в Kbit/s.
    pub(crate) tbr: Option<f64>,
    /// Video bitrate в Kbit/s.
    pub(crate) vbr: Option<f64>,
    /// Audio bitrate в Kbit/s.
    pub(crate) abr: Option<f64>,
    /// Audio sample rate в Hz.
    pub(crate) asr: Option<f64>,
    /// Число audio channels.
    pub(crate) audio_channels: Option<u16>,
    /// Optional audio language.
    pub(crate) language: Option<String>,
    /// Typed dynamic-range hint.
    pub(crate) dynamic_range: Option<String>,
    /// Format-level DRM marker.
    pub(crate) has_drm: Option<bool>,
    /// Bounded serialized fragments либо lossy repr.
    pub(crate) fragments: Option<Value>,
    /// Base locator для relative fragments.
    pub(crate) fragment_base_url: Option<String>,
    /// Inline HLS media playlist.
    pub(crate) hls_media_playlist_data: Option<String>,
    /// Transient HTTP headers.
    pub(crate) http_headers: Option<Value>,
    /// Scoped serialized cookies.
    pub(crate) cookies: Option<Value>,
    /// Serialized request body; S00 target rows исключают его использование.
    pub(crate) request_data: Option<Value>,
    /// Query material для media segments.
    pub(crate) extra_param_to_segment_url: Option<String>,
    /// Query material для encryption keys.
    pub(crate) extra_param_to_key_url: Option<String>,
    /// Extractor-provided HLS AES material.
    pub(crate) hls_aes: Option<Value>,
    /// Browser fingerprint requirement.
    pub(crate) impersonate: Option<Value>,
    /// Internal downloader state, которое Rustiplayer никогда не исполняет.
    pub(crate) downloader_options: Option<Value>,
    /// Private BunnyCDN state из pinned source.
    #[serde(rename = "_bunnycdn_ping_data")]
    pub(crate) bunnycdn_ping_data: Option<Value>,
    /// Private mutable cookie refresh state из pinned source.
    #[serde(rename = "_cookie_refresh_params")]
    pub(crate) cookie_refresh_params: Option<Value>,
    /// RTMP page locator.
    pub(crate) page_url: Option<String>,
    /// RTMP application identity.
    pub(crate) app: Option<String>,
    /// RTMP play path.
    pub(crate) play_path: Option<String>,
    /// RTMP tcUrl.
    pub(crate) tc_url: Option<String>,
    /// RTMP Flash version identity.
    pub(crate) flash_version: Option<String>,
    /// RTMP live flag.
    pub(crate) rtmp_live: Option<bool>,
    /// RTMP connection arguments.
    pub(crate) rtmp_conn: Option<Value>,
    /// Exact RTMP protocol identity.
    pub(crate) rtmp_protocol: Option<String>,
    /// RTMP real-time flag.
    pub(crate) rtmp_real_time: Option<bool>,
}
