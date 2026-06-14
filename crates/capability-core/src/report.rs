use std::time::{SystemTime, UNIX_EPOCH};

use codec_core::{
    DecodeBackendId, SupportedVideoDecodeFormat, VideoDecodeRequirement, ZeroCopyExportRequirement,
    video_frame_pixel_layout_from_decode_requirement,
};
use render_core::RenderCapabilities;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract, VideoFramePixelLayout};

/// Версия JSON/report схемы capability layer.
pub type CapabilitySchemaVersion = u32;

/// Текущая версия capability report.
pub const CURRENT_CAPABILITY_SCHEMA_VERSION: CapabilitySchemaVersion = 4;

/// Provider, который умеет построить capabilities для одного video backend.
pub trait VideoCapabilityProvider {
    /// Возвращает стабильный backend id без запуска probe.
    fn backend_id(&self) -> DecodeBackendId;

    /// Выполняет probe backend-а и возвращает typed result.
    fn probe(&self) -> BackendCapabilities;
}

/// Агрегатор backend probes.
#[derive(Default)]
pub struct CapabilityScanner {
    /// Список backend providers, зарегистрированных compile-time.
    providers: Vec<Box<dyn VideoCapabilityProvider>>,

    /// Render capabilities, полученные от уже созданных renderer backend-ов.
    render_backends: Vec<RenderCapabilities>,
}

impl CapabilityScanner {
    /// Создаёт пустой scanner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Регистрирует backend provider.
    pub fn register_provider(&mut self, provider: Box<dyn VideoCapabilityProvider>) {
        self.providers.push(provider);
    }

    /// Регистрирует capabilities renderer backend-а.
    pub fn register_render_capabilities(&mut self, capabilities: RenderCapabilities) {
        self.render_backends.push(capabilities);
    }

    /// Запускает все probes и возвращает системный report.
    #[must_use]
    pub fn scan(&self) -> SystemCapabilities {
        let probed_at_unix_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);

        self.scan_with_timestamp(probed_at_unix_seconds)
    }

    /// Запускает probes с заданным timestamp, чтобы tests были deterministic.
    #[must_use]
    pub fn scan_with_timestamp(&self, probed_at_unix_seconds: u64) -> SystemCapabilities {
        let mut video_backends = Vec::with_capacity(self.providers.len());

        for provider in &self.providers {
            let backend_id = provider.backend_id();
            debug!(backend = %backend_id, "Запуск capability probe");
            let mut capabilities = provider.probe();
            if capabilities.backend_id != backend_id {
                warn!(
                    expected = %backend_id,
                    actual = %capabilities.backend_id,
                    "Capability provider вернул другой backend id"
                );
            }
            capabilities =
                filter_backend_capabilities_for_report(capabilities, &self.render_backends);
            video_backends.push(capabilities);
        }

        SystemCapabilities {
            schema_version: CURRENT_CAPABILITY_SCHEMA_VERSION,
            probed_at_unix_seconds,
            video_backends,
            render_backends: self.render_backends.clone(),
        }
    }
}

/// Оставляет в system report только formats, которые реально проходят production intersection.
fn filter_backend_capabilities_for_report(
    mut capabilities: BackendCapabilities,
    render_backends: &[RenderCapabilities],
) -> BackendCapabilities {
    if !capabilities.status.is_available() {
        return capabilities;
    }

    let original_format_count = capabilities.supported_video_decode_formats.len();
    let export_paths = capabilities.export_paths.clone();
    let p010_storage_layouts = capabilities.p010_storage_layouts.clone();
    capabilities
        .supported_video_decode_formats
        .retain(|format| {
            reportable_decode_format(
                format,
                &export_paths,
                &p010_storage_layouts,
                render_backends,
            )
        });

    let hidden_format_count =
        original_format_count.saturating_sub(capabilities.supported_video_decode_formats.len());
    if hidden_format_count > 0 {
        capabilities.diagnostics.push(format!(
            "Capability report hid {hidden_format_count} decode formats without full zero-copy renderer intersection"
        ));
    }

    capabilities
}

/// Проверяет hardware+export+renderer часть capability report intersection.
fn reportable_decode_format(
    format: &SupportedVideoDecodeFormat,
    export_paths: &[VideoExportPath],
    p010_storage_layouts: &[DmaBufImageLayout],
    render_backends: &[RenderCapabilities],
) -> bool {
    if !export_paths.contains(&VideoExportPath::DmaBuf) {
        return false;
    }

    let requirement = decode_requirement_for_supported_format(format);
    let frame_contracts = reportable_frame_contracts(format, p010_storage_layouts);
    render_backends.iter().any(|renderer| {
        frame_contracts
            .iter()
            .any(|contract| renderer.supports_video_output(&requirement, *contract))
    })
}

/// Строит current production frame contracts для capability report filtering.
fn reportable_frame_contracts(
    format: &SupportedVideoDecodeFormat,
    p010_storage_layouts: &[DmaBufImageLayout],
) -> Vec<VideoFrameContract> {
    match video_frame_pixel_layout_from_decode_requirement(
        &decode_requirement_for_supported_format(format),
    ) {
        Some(VideoFramePixelLayout::Nv12) => {
            vec![VideoFrameContract::dma_buf_nv12(
                DmaBufImageLayout::SeparateLayers,
            )]
        }
        Some(VideoFramePixelLayout::P010) => {
            let layouts = if p010_storage_layouts.is_empty() {
                vec![DmaBufImageLayout::SeparateLayers]
            } else {
                p010_storage_layouts.to_vec()
            };
            layouts
                .into_iter()
                .map(VideoFrameContract::dma_buf_p010)
                .collect()
        }
        Some(
            VideoFramePixelLayout::Yuv420Planar8
            | VideoFramePixelLayout::Yuv420Planar10Le
            | VideoFramePixelLayout::Rgba8,
        )
        | None => Vec::new(),
    }
}

/// Собирает минимальное stream requirement из probed backend format-а для renderer check-а.
fn decode_requirement_for_supported_format(
    format: &SupportedVideoDecodeFormat,
) -> VideoDecodeRequirement {
    let mut requirement = VideoDecodeRequirement::new(format.codec)
        .with_profile(format.profile)
        .with_bit_depth(format.bit_depth)
        .with_chroma(format.chroma);
    requirement.surface_format = video_frame_pixel_layout_from_decode_requirement(&requirement);
    requirement.hdr = format.hdr_input;
    requirement
}

/// Полный capability report текущей системы.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SystemCapabilities {
    /// Версия report schema.
    pub schema_version: CapabilitySchemaVersion,

    /// Unix timestamp, когда выполнялся probe.
    pub probed_at_unix_seconds: u64,

    /// Capabilities всех video decode backend-ов.
    pub video_backends: Vec<BackendCapabilities>,

    /// Capabilities renderer backend-ов, доступных shell-слою.
    pub render_backends: Vec<RenderCapabilities>,
}

impl SystemCapabilities {
    /// Создаёт report без backend-ов.
    #[must_use]
    pub fn empty(probed_at_unix_seconds: u64) -> Self {
        Self {
            schema_version: CURRENT_CAPABILITY_SCHEMA_VERSION,
            probed_at_unix_seconds,
            video_backends: Vec::new(),
            render_backends: Vec::new(),
        }
    }

    /// Возвращает все supported video formats из доступных backend-ов.
    pub fn supported_video_formats(&self) -> impl Iterator<Item = &SupportedVideoDecodeFormat> {
        self.video_backends
            .iter()
            .filter(|backend| backend.status.is_available())
            .flat_map(|backend| backend.supported_video_decode_formats.iter())
    }

    /// Формирует короткую сводку для верхнего UI.
    #[must_use]
    pub fn summary_text(&self) -> String {
        if self.video_backends.is_empty() && self.render_backends.is_empty() {
            return "Capability probe: backend-ы не зарегистрированы".to_string();
        }

        let available_backends = self
            .video_backends
            .iter()
            .filter(|backend| backend.status.is_available())
            .count();
        let supported_formats = self.supported_video_formats().count();

        format!(
            "Capability probe: {available_backends}/{} video backend доступно, {supported_formats} decode formats, {} render backend",
            self.video_backends.len(),
            self.render_backends.len()
        )
    }

    /// Формирует многострочный report для telemetry panel и логов.
    #[must_use]
    pub fn detailed_report_text(&self) -> String {
        if self.video_backends.is_empty() {
            return self.summary_text();
        }

        let mut lines = vec![self.summary_text()];
        for backend in &self.video_backends {
            lines.push(backend.summary_text());
            for format in backend.supported_video_decode_formats.iter().take(12) {
                lines.push(format!("  - {}", format.describe()));
            }
            if backend.supported_video_decode_formats.len() > 12 {
                lines.push(format!(
                    "  - ... ещё {} formats",
                    backend.supported_video_decode_formats.len() - 12
                ));
            }
        }
        for renderer in &self.render_backends {
            lines.push(renderer.summary_text());
        }

        lines.join("\n")
    }
}

/// Возможности одного backend-а.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BackendCapabilities {
    /// Стабильный backend id.
    pub backend_id: DecodeBackendId,

    /// Человекочитаемое имя backend-а.
    pub display_name: String,

    /// Доступность backend-а после probe.
    pub status: BackendProbeStatus,

    /// Информация о драйвере и устройстве.
    pub driver: BackendDriverInfo,

    /// Typed supported decode matrix.
    pub supported_video_decode_formats: Vec<SupportedVideoDecodeFormat>,

    /// Сырые profile labels для диагностики backend-specific расхождений.
    pub raw_profiles: Vec<String>,

    /// Сырые entrypoint labels для диагностики.
    pub raw_entrypoints: Vec<String>,

    /// Сырые RT format labels для диагностики.
    pub raw_rt_formats: Vec<String>,

    /// Known quirks backend-а.
    pub quirks: Vec<DriverQuirk>,

    /// Export/upload paths, которые probe или runtime считает доступными.
    pub export_paths: Vec<VideoExportPath>,

    /// P010 DMA-BUF layouts, которые backend ожидает на decoder/renderer boundary.
    #[serde(default)]
    pub p010_storage_layouts: Vec<DmaBufImageLayout>,

    /// Неструктурированные diagnostic notes, не влияющие на selection.
    pub diagnostics: Vec<String>,
}

impl BackendCapabilities {
    /// Создаёт unavailable backend report с причиной.
    #[must_use]
    pub fn unavailable(
        backend_id: DecodeBackendId,
        display_name: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            backend_id,
            display_name: display_name.into(),
            status: BackendProbeStatus::Unavailable {
                reason: reason.into(),
            },
            driver: BackendDriverInfo::default(),
            supported_video_decode_formats: Vec::new(),
            raw_profiles: Vec::new(),
            raw_entrypoints: Vec::new(),
            raw_rt_formats: Vec::new(),
            quirks: Vec::new(),
            export_paths: Vec::new(),
            p010_storage_layouts: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Формирует одну строку report для backend-а.
    #[must_use]
    pub fn summary_text(&self) -> String {
        let zero_copy_export_label = if self.export_paths.contains(&VideoExportPath::DmaBuf) {
            "DMA-BUF zero-copy"
        } else {
            "zero-copy export unavailable"
        };

        match &self.status {
            BackendProbeStatus::Available => format!(
                "{}: доступен, {} decode formats, export: {}{}",
                self.display_name,
                self.supported_video_decode_formats.len(),
                zero_copy_export_label,
                self.driver
                    .vendor
                    .as_ref()
                    .map(|vendor| format!(", vendor: {vendor}"))
                    .unwrap_or_default()
            ),
            BackendProbeStatus::Unavailable { reason } => {
                format!("{}: недоступен ({reason})", self.display_name)
            }
        }
    }
}

/// Состояние backend-а после probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendProbeStatus {
    /// Backend доступен и вернул capability matrix.
    Available,

    /// Backend недоступен; причина пригодна для UI/log.
    Unavailable {
        /// Человекочитаемая причина.
        reason: String,
    },
}

impl BackendProbeStatus {
    /// Возвращает `true`, если backend доступен.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Информация о драйвере и устройстве backend-а.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BackendDriverInfo {
    /// Vendor string из backend API.
    pub vendor: Option<String>,

    /// Нормализованное имя драйверного семейства, например `intel-ihd`.
    pub driver_name: Option<String>,

    /// Device name, если backend API его сообщил.
    pub device_name: Option<String>,
}

/// Known quirk, который влияет на диагностику или будущую selection policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DriverQuirk {
    /// Стабильный id quirk-а.
    pub id: String,

    /// Описание для diagnostics.
    pub description: String,
}

/// Compatibility alias: capability layer использует общий zero-copy export contract.
pub type VideoExportPath = ZeroCopyExportRequirement;

#[cfg(test)]
mod tests {
    use codec_core::{BitDepth, ChromaSubsampling, VideoCodec, VideoProfile, Vp9Profile};

    use super::*;

    /// Static provider для scanner-level filtering tests без реального backend probe.
    struct StaticVideoProvider {
        /// Capabilities, которые provider вернёт scanner-у.
        capabilities: BackendCapabilities,
    }

    impl VideoCapabilityProvider for StaticVideoProvider {
        /// Возвращает backend id тестового report-а.
        fn backend_id(&self) -> DecodeBackendId {
            self.capabilities.backend_id.clone()
        }

        /// Возвращает заранее заданные capabilities.
        fn probe(&self) -> BackendCapabilities {
            self.capabilities.clone()
        }
    }

    /// Собирает минимальный available backend report для filtering tests.
    fn backend_with_formats(
        formats: Vec<SupportedVideoDecodeFormat>,
        export_paths: Vec<VideoExportPath>,
    ) -> BackendCapabilities {
        BackendCapabilities {
            backend_id: DecodeBackendId::vaapi(),
            display_name: "Test VA-API".to_string(),
            status: BackendProbeStatus::Available,
            driver: BackendDriverInfo::default(),
            supported_video_decode_formats: formats,
            raw_profiles: Vec::new(),
            raw_entrypoints: Vec::new(),
            raw_rt_formats: Vec::new(),
            quirks: Vec::new(),
            export_paths,
            p010_storage_layouts: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Собирает one-line decode format с production VAAPI backend id.
    fn vp9_format(
        profile: Vp9Profile,
        bit_depth: BitDepth,
        chroma: ChromaSubsampling,
        hdr_input: bool,
    ) -> SupportedVideoDecodeFormat {
        SupportedVideoDecodeFormat {
            codec: VideoCodec::Vp9,
            profile: VideoProfile::Vp9(profile),
            bit_depth,
            chroma,
            max_width: Some(3840),
            max_height: Some(2160),
            max_fps: None,
            hdr_input,
            backend: DecodeBackendId::vaapi(),
        }
    }

    /// Проверяет, что system report не рекламирует formats без DMA-BUF export.
    #[test]
    fn scanner_hides_formats_without_dma_buf_export() {
        let mut scanner = CapabilityScanner::new();
        scanner.register_provider(Box::new(StaticVideoProvider {
            capabilities: backend_with_formats(
                vec![vp9_format(
                    Vp9Profile::Profile0,
                    BitDepth::Eight,
                    ChromaSubsampling::Yuv420,
                    false,
                )],
                Vec::new(),
            ),
        }));
        scanner.register_render_capabilities(RenderCapabilities::wgpu_nv12(Some(4096)));

        let report = scanner.scan_with_timestamp(7);

        assert_eq!(report.supported_video_formats().count(), 0);
        assert!(
            report.video_backends[0]
                .diagnostics
                .iter()
                .any(|message| message.contains("zero-copy renderer intersection"))
        );
    }

    /// Проверяет, что renderer-incompatible P010 не попадает в advertised decode matrix.
    #[test]
    fn scanner_hides_renderer_incompatible_surface_formats() {
        let mut scanner = CapabilityScanner::new();
        scanner.register_provider(Box::new(StaticVideoProvider {
            capabilities: backend_with_formats(
                vec![
                    vp9_format(
                        Vp9Profile::Profile0,
                        BitDepth::Eight,
                        ChromaSubsampling::Yuv420,
                        false,
                    ),
                    vp9_format(
                        Vp9Profile::Profile2,
                        BitDepth::Ten,
                        ChromaSubsampling::Yuv420,
                        true,
                    ),
                ],
                vec![VideoExportPath::DmaBuf],
            ),
        }));
        scanner.register_render_capabilities(RenderCapabilities::wgpu_nv12(Some(4096)));

        let report = scanner.scan_with_timestamp(7);
        let formats = report.supported_video_formats().collect::<Vec<_>>();

        assert_eq!(formats.len(), 1);
        assert_eq!(formats[0].profile, VideoProfile::Vp9(Vp9Profile::Profile0));
    }

    /// Проверяет positive path: NV12 + DMA-BUF + renderer support остаётся advertised.
    #[test]
    fn scanner_keeps_renderer_compatible_zero_copy_format() {
        let mut scanner = CapabilityScanner::new();
        scanner.register_provider(Box::new(StaticVideoProvider {
            capabilities: backend_with_formats(
                vec![vp9_format(
                    Vp9Profile::Profile0,
                    BitDepth::Eight,
                    ChromaSubsampling::Yuv420,
                    false,
                )],
                vec![VideoExportPath::DmaBuf],
            ),
        }));
        scanner.register_render_capabilities(RenderCapabilities::wgpu_nv12(Some(4096)));

        let report = scanner.scan_with_timestamp(7);

        assert_eq!(report.supported_video_formats().count(), 1);
        assert!(report.video_backends[0].diagnostics.is_empty());
    }
}
