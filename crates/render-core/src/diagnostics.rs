use std::fmt;

use serde::{Deserialize, Serialize};
use video_frame_contract::{
    DmaBufImageLayout, VideoFrameContract, VideoFrameContractValidationError,
    VideoFramePixelLayout, VideoFrameTransferPath,
};

use crate::{ActiveColorPath, P010RenderReadiness};

/// Источник optional HDR metadata, который renderer использовал для diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HdrMetadataDiagnosticMarker {
    /// Поле не применимо к текущему color path.
    NotApplicable,

    /// Значение пришло из container/bitstream/backend metadata.
    Confirmed,

    /// Значение заменено documented reference default-ом.
    ReferenceDefault,
}

impl HdrMetadataDiagnosticMarker {
    /// Возвращает стабильную подпись для telemetry panel.
    #[must_use]
    pub const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::NotApplicable => "not-applicable",
            Self::Confirmed => "confirmed",
            Self::ReferenceDefault => "reference-default",
        }
    }
}

impl fmt::Display for HdrMetadataDiagnosticMarker {
    /// Печатает marker без UI-специфичного форматирования.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.diagnostic_label())
    }
}

/// Source markers для optional HDR metadata, использованной HDR-to-SDR path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HdrReferenceDefaultDiagnostics {
    /// Mastering display max luminance source.
    pub mastering_max_luminance: HdrMetadataDiagnosticMarker,

    /// Mastering display min luminance source.
    pub mastering_min_luminance: HdrMetadataDiagnosticMarker,

    /// MaxCLL source.
    pub max_content_light_level: HdrMetadataDiagnosticMarker,

    /// MaxFALL source.
    pub max_frame_average_light_level: HdrMetadataDiagnosticMarker,
}

impl HdrReferenceDefaultDiagnostics {
    /// Возвращает `true`, если хотя бы одно поле взято из reference defaults.
    #[must_use]
    pub const fn has_reference_defaults(&self) -> bool {
        matches!(
            self.mastering_max_luminance,
            HdrMetadataDiagnosticMarker::ReferenceDefault
        ) || matches!(
            self.mastering_min_luminance,
            HdrMetadataDiagnosticMarker::ReferenceDefault
        ) || matches!(
            self.max_content_light_level,
            HdrMetadataDiagnosticMarker::ReferenceDefault
        ) || matches!(
            self.max_frame_average_light_level,
            HdrMetadataDiagnosticMarker::ReferenceDefault
        )
    }

    /// Формирует compact diagnostics string для UI.
    #[must_use]
    pub fn diagnostic_text(&self) -> String {
        format!(
            "mastering-max={}, mastering-min={}, maxcll={}, maxfall={}",
            self.mastering_max_luminance,
            self.mastering_min_luminance,
            self.max_content_light_level,
            self.max_frame_average_light_level
        )
    }
}

/// Renderer-neutral diagnostics, которые UI может читать без GPU handles.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderDiagnostics {
    /// Последний color path, реально выбранный renderer-ом для video frame.
    pub active_color_path: Option<ActiveColorPath>,

    /// Source markers optional HDR metadata для последнего HDR-to-SDR frame.
    #[serde(default)]
    pub hdr_reference_defaults: Option<HdrReferenceDefaultDiagnostics>,

    /// Количество scissor draw rects последнего video pass (1 без exclusion rects,
    /// 0 если video pass не рисовал кадр).
    #[serde(default)]
    pub video_draw_rect_count: usize,
}

impl RenderDiagnostics {
    /// Возвращает строку active color path для telemetry panel.
    #[must_use]
    pub fn active_color_path_text(&self) -> Option<String> {
        self.active_color_path
            .as_ref()
            .map(ActiveColorPath::diagnostic_text)
    }

    /// Возвращает source markers optional HDR metadata для telemetry panel.
    #[must_use]
    pub fn hdr_reference_defaults_text(&self) -> Option<String> {
        self.hdr_reference_defaults
            .as_ref()
            .map(HdrReferenceDefaultDiagnostics::diagnostic_text)
    }
}

/// Техническая причина отказа при проверке одного renderer frame contract-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFrameContractRejection {
    /// Сам contract нарушает invariant neutral vocabulary.
    InvalidContract {
        /// Причина, которую вернул `video-frame-contract`.
        reason: VideoFrameContractValidationError,
    },

    /// Renderer вообще не объявлял такой transfer path.
    UnsupportedTransferPath {
        /// Transfer path/layout, который запросил caller.
        transfer_path: VideoFrameTransferPath,
    },

    /// Renderer не объявлял такой pixel layout ни для одного path-а.
    UnsupportedPixelLayout {
        /// Pixel layout, который запросил caller.
        pixel_layout: VideoFramePixelLayout,
    },

    /// Renderer поддерживает DMA-BUF для pixel layout-а, но не этот image layout.
    UnsupportedDmaBufImageLayout {
        /// Pixel layout, для которого проверялся DMA-BUF layout.
        pixel_layout: VideoFramePixelLayout,

        /// DMA-BUF image layout, который не входит в renderer contract list.
        image_layout: DmaBufImageLayout,
    },

    /// Pixel layout и transfer path по отдельности известны, но не как одна пара.
    UnsupportedContractCombination {
        /// Полный frame contract, который нельзя собирать через Cartesian product.
        frame_contract: VideoFrameContract,
    },
}

/// Размерная ось, которая превысила renderer texture limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderTextureDimension {
    /// Coded width stream-а.
    Width,

    /// Coded height stream-а.
    Height,
}

/// Техническая причина отказа stream-level renderer output check-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderVideoOutputRejection {
    /// Frame contract сам по себе не входит в renderer input boundary.
    FrameContract {
        /// Детальная contract-level причина.
        reason: RenderFrameContractRejection,
    },

    /// P010 path объявлен не как production-renderable.
    P010NotRenderable {
        /// Текущий diagnostic readiness.
        readiness: P010RenderReadiness,
    },

    /// Stream требует HDR обработки, но renderer не имеет подходящего output path-а.
    HdrUnsupported {
        /// Frame contract, который проверялся для HDR stream-а.
        frame_contract: VideoFrameContract,
    },

    /// Coded размер stream-а превышает renderer texture limit.
    MaxTextureSizeExceeded {
        /// Какая ось превысила limit.
        dimension: RenderTextureDimension,

        /// Запрошенный размер по этой оси.
        requested: u32,

        /// Максимум, объявленный renderer backend-ом.
        max_texture_size: u32,
    },
}
#[cfg(test)]
#[path = "tests/diagnostics.rs"]
mod tests;
