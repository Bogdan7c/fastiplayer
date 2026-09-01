//! Type-erased open payload, видимый только preparation boundary.

use super::*;

/// Внутренний adapter payload нужен только `media_open::preparation` и focused tests.
pub(crate) enum WebMediaOpenAdapterView {
    /// Direct progressive resource без adaptive catalog-а.
    Direct {
        locator: service_direct_media::DirectMediaUrl,
        network_config: rustiplayer_config::NetworkConfig,
        demux_config: rustiplayer_config::PlayerDemuxConfig,
    },
    /// Native HLS root и provider-neutral intent.
    NativeHls {
        source: NativeHlsUrl,
        intent: NativeHlsOpenIntent,
        settings: WebMediaOpenSettings,
    },
    /// Native static DASH root и provider-neutral intent.
    NativeDash {
        source: NativeDashUrl,
        intent: NativeDashOpenIntent,
        settings: WebMediaOpenSettings,
    },
    /// Native HDS stable F4M root и provider-neutral intent.
    NativeHds {
        source: NativeHdsUrl,
        intent: NativeHdsOpenIntent,
        settings: WebMediaOpenSettings,
    },
    /// Native Smooth stable root и provider-neutral intent.
    NativeSmooth {
        source: NativeSmoothUrl,
        intent: NativeSmoothOpenIntent,
        settings: WebMediaOpenSettings,
    },
    /// Extractor locator и neutral candidate selection intent.
    Extractor {
        locator: service_ytdlp::YtDlpMediaLocator,
        selection_intent: crate::web_media_open::YtDlpCandidateOpenIntent,
        settings: WebMediaOpenSettings,
    },
}

impl From<WebMediaOpenAdapter> for WebMediaOpenAdapterView {
    fn from(adapter: WebMediaOpenAdapter) -> Self {
        match adapter {
            WebMediaOpenAdapter::Direct {
                locator,
                network_config,
                demux_config,
            } => Self::Direct {
                locator,
                network_config,
                demux_config,
            },
            WebMediaOpenAdapter::NativeHls {
                source,
                intent,
                settings,
            } => Self::NativeHls {
                source,
                intent,
                settings,
            },
            WebMediaOpenAdapter::NativeDash {
                source,
                intent,
                settings,
            } => Self::NativeDash {
                source,
                intent,
                settings,
            },
            WebMediaOpenAdapter::NativeHds {
                source,
                intent,
                settings,
            } => Self::NativeHds {
                source,
                intent,
                settings,
            },
            WebMediaOpenAdapter::NativeSmooth {
                source,
                intent,
                settings,
            } => Self::NativeSmooth {
                source,
                intent,
                settings,
            },
            WebMediaOpenAdapter::Extractor {
                locator,
                selection_intent,
                settings,
            } => Self::Extractor {
                locator,
                selection_intent,
                settings,
            },
        }
    }
}
