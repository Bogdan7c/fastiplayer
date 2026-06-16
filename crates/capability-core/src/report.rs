use std::time::{SystemTime, UNIX_EPOCH};

use codec_core::{DecodeBackendId, SupportedVideoDecodeFormat, VideoDecodeRequirement};
use render_core::RenderCapabilities;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use video_frame_contract::{VideoFrameContract, VideoFrameTransferPath};

/// Версия JSON/report схемы capability layer.
pub type CapabilitySchemaVersion = u32;

/// Текущая версия capability report.
pub const CURRENT_CAPABILITY_SCHEMA_VERSION: CapabilitySchemaVersion = 5;

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
        let mut playable_video_outputs = Vec::new();

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
            playable_video_outputs.extend(playable_outputs_for_backend(
                &mut capabilities,
                &self.render_backends,
            ));
            video_backends.push(capabilities);
        }

        SystemCapabilities {
            schema_version: CURRENT_CAPABILITY_SCHEMA_VERSION,
            probed_at_unix_seconds,
            video_backends,
            render_backends: self.render_backends.clone(),
            playable_video_outputs,
        }
    }
}

/// Собирает system-level outputs, которые проходят backend/provider + renderer intersection.
fn playable_outputs_for_backend(
    capabilities: &mut BackendCapabilities,
    render_backends: &[RenderCapabilities],
) -> Vec<SupportedVideoOutput> {
    if !capabilities.status.is_available() {
        return Vec::new();
    }

    let playable_outputs = capabilities
        .raw_supported_outputs
        .iter()
        .filter(|output| output.backend == capabilities.backend_id)
        .filter(|output| playable_video_output(output, render_backends))
        .cloned()
        .collect::<Vec<_>>();

    let hidden_output_count = capabilities
        .raw_supported_outputs
        .len()
        .saturating_sub(playable_outputs.len());
    if hidden_output_count > 0 {
        capabilities.diagnostics.push(format!(
            "Capability report found {hidden_output_count} raw video outputs without renderer transfer/layout intersection"
        ));
    }

    playable_outputs
}

/// Проверяет backend-declared output against renderer capabilities.
fn playable_video_output(
    output: &SupportedVideoOutput,
    render_backends: &[RenderCapabilities],
) -> bool {
    let requirement = decode_requirement_for_supported_format(&output.decode_format);
    render_backends
        .iter()
        .any(|renderer| renderer.supports_video_output(&requirement, output.frame_contract))
}

/// Собирает минимальное stream requirement из probed backend format-а для renderer check-а.
fn decode_requirement_for_supported_format(
    format: &SupportedVideoDecodeFormat,
) -> VideoDecodeRequirement {
    let mut requirement = VideoDecodeRequirement::new(format.codec)
        .with_profile(format.profile)
        .with_bit_depth(format.bit_depth)
        .with_chroma(format.chroma);
    requirement.hdr = format.hdr_input;
    requirement
}

/// Один concrete decoded output, который backend/provider может произвести.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SupportedVideoOutput {
    /// Backend, который владеет decode/output path-ом.
    pub backend: DecodeBackendId,

    /// Codec-level decode capability без backend ownership.
    pub decode_format: SupportedVideoDecodeFormat,

    /// Concrete decoder -> renderer frame contract для этого output-а.
    pub frame_contract: VideoFrameContract,
}

impl SupportedVideoOutput {
    /// Проверяет, закрывает ли output stream requirement на codec уровне.
    #[must_use]
    pub fn satisfies(&self, requirement: &VideoDecodeRequirement) -> bool {
        self.decode_format.satisfies(requirement)
    }

    /// Возвращает transfer path, объявленный provider-ом для этого output-а.
    #[must_use]
    pub const fn transfer_path(&self) -> VideoFrameTransferPath {
        self.frame_contract.transfer_path
    }

    /// Формирует компактное описание output-а для diagnostics.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} via {}",
            self.decode_format.describe(),
            self.frame_contract.diagnostic_label()
        )
    }
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

    /// Outputs, которые прошли system-level renderer intersection.
    pub playable_video_outputs: Vec<SupportedVideoOutput>,
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
            playable_video_outputs: Vec::new(),
        }
    }

    /// Возвращает все raw provider-declared outputs из доступных backend-ов.
    pub fn raw_video_outputs(&self) -> impl Iterator<Item = &SupportedVideoOutput> {
        self.video_backends
            .iter()
            .filter(|backend| backend.status.is_available())
            .flat_map(|backend| backend.raw_supported_outputs.iter())
    }

    /// Возвращает все playable system-level outputs.
    pub fn supported_video_outputs(&self) -> impl Iterator<Item = &SupportedVideoOutput> {
        self.playable_video_outputs.iter()
    }

    /// Возвращает codec-only summaries для compatibility call sites.
    pub fn supported_video_formats(&self) -> impl Iterator<Item = &SupportedVideoDecodeFormat> {
        self.playable_video_outputs
            .iter()
            .map(|output| &output.decode_format)
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
        let playable_outputs = self.supported_video_outputs().count();
        let raw_outputs = self.raw_video_outputs().count();

        format!(
            "Capability probe: {available_backends}/{} video backend доступно, {playable_outputs}/{raw_outputs} video outputs playable, {} render backend",
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
            for output in backend.raw_supported_outputs.iter().take(12) {
                let playable_label = if self.playable_video_outputs.contains(output) {
                    "playable"
                } else {
                    "raw only"
                };
                lines.push(format!("  - {} ({playable_label})", output.describe()));
            }
            if backend.raw_supported_outputs.len() > 12 {
                lines.push(format!(
                    "  - ... ещё {} outputs",
                    backend.raw_supported_outputs.len() - 12
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

    /// Raw provider-declared outputs до renderer intersection.
    pub raw_supported_outputs: Vec<SupportedVideoOutput>,

    /// Сырые profile labels для диагностики backend-specific расхождений.
    pub raw_profiles: Vec<String>,

    /// Сырые entrypoint labels для диагностики.
    pub raw_entrypoints: Vec<String>,

    /// Сырые RT format labels для диагностики.
    pub raw_rt_formats: Vec<String>,

    /// Known quirks backend-а.
    pub quirks: Vec<DriverQuirk>,

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
            raw_supported_outputs: Vec::new(),
            raw_profiles: Vec::new(),
            raw_entrypoints: Vec::new(),
            raw_rt_formats: Vec::new(),
            quirks: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Формирует одну строку report для backend-а.
    #[must_use]
    pub fn summary_text(&self) -> String {
        let transfer_summary = summarize_output_transfer_paths(&self.raw_supported_outputs);

        match &self.status {
            BackendProbeStatus::Available => format!(
                "{}: доступен, {} raw outputs, transfer: {}{}",
                self.display_name,
                self.raw_supported_outputs.len(),
                transfer_summary,
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

/// Формирует backend-level transfer summary без отдельного source-of-truth списка.
fn summarize_output_transfer_paths(outputs: &[SupportedVideoOutput]) -> String {
    if outputs.is_empty() {
        return "нет output transfer paths".to_string();
    }

    let mut labels = outputs
        .iter()
        .map(|output| output.transfer_path().diagnostic_label().to_string())
        .collect::<Vec<_>>();
    labels.sort();
    labels.dedup();
    labels.join(", ")
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

#[cfg(test)]
mod tests {
    use codec_core::{BitDepth, ChromaSubsampling, VideoCodec, VideoProfile, Vp9Profile};
    use render_core::{HdrOutputMode, P010RenderReadiness, RenderBackendKind, UiCompositionMode};
    use video_frame_contract::{DmaBufImageLayout, VideoFramePixelLayout};

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
    fn backend_with_outputs(outputs: Vec<SupportedVideoOutput>) -> BackendCapabilities {
        backend_with_id_and_outputs(DecodeBackendId::vaapi(), "Test VA-API", outputs)
    }

    /// Собирает минимальный available backend report с explicit backend id.
    fn backend_with_id_and_outputs(
        backend_id: DecodeBackendId,
        display_name: &str,
        outputs: Vec<SupportedVideoOutput>,
    ) -> BackendCapabilities {
        BackendCapabilities {
            backend_id,
            display_name: display_name.to_string(),
            status: BackendProbeStatus::Available,
            driver: BackendDriverInfo::default(),
            raw_supported_outputs: outputs,
            raw_profiles: Vec::new(),
            raw_entrypoints: Vec::new(),
            raw_rt_formats: Vec::new(),
            quirks: Vec::new(),
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
        }
    }

    /// Собирает output для тестового VAAPI backend-а.
    fn output(
        decode_format: SupportedVideoDecodeFormat,
        frame_contract: VideoFrameContract,
    ) -> SupportedVideoOutput {
        output_for_backend(DecodeBackendId::vaapi(), decode_format, frame_contract)
    }

    /// Собирает output для заданного backend-а.
    fn output_for_backend(
        backend: DecodeBackendId,
        decode_format: SupportedVideoDecodeFormat,
        frame_contract: VideoFrameContract,
    ) -> SupportedVideoOutput {
        SupportedVideoOutput {
            backend,
            decode_format,
            frame_contract,
        }
    }

    /// Собирает fake renderer, который объявляет только exact frame contracts.
    fn fake_renderer_with_contracts(
        supported_frame_contracts: Vec<VideoFrameContract>,
    ) -> RenderCapabilities {
        RenderCapabilities {
            backend: RenderBackendKind::Wgpu,
            display_name: "Fake renderer".to_string(),
            supported_frame_contracts,
            p010_render_readiness: P010RenderReadiness::Unavailable,
            supported_hdr_to_sdr_operators: Vec::new(),
            hdr_output_mode: HdrOutputMode::SdrBt709Only,
            supports_hdr_to_sdr: false,
            supports_native_hdr_output: false,
            max_texture_size: Some(4096),
            advanced_ui: false,
            ui_composition_mode: UiCompositionMode::Overlay,
            present_timing_metrics: false,
        }
    }

    /// Проверяет, что raw output остаётся, но не становится playable без renderer transfer.
    #[test]
    fn scanner_keeps_raw_outputs_without_renderer_transfer_as_unplayable() {
        let mut scanner = CapabilityScanner::new();
        scanner.register_provider(Box::new(StaticVideoProvider {
            capabilities: backend_with_outputs(vec![output(
                vp9_format(
                    Vp9Profile::Profile0,
                    BitDepth::Eight,
                    ChromaSubsampling::Yuv422,
                    false,
                ),
                VideoFrameContract {
                    pixel_layout: VideoFramePixelLayout::Yuv422Planar8,
                    transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
                },
            )]),
        }));
        scanner.register_render_capabilities(fake_renderer_with_contracts(vec![
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
        ]));

        let report = scanner.scan_with_timestamp(7);

        assert_eq!(report.raw_video_outputs().count(), 1);
        assert_eq!(report.supported_video_outputs().count(), 0);
        assert!(
            report.video_backends[0]
                .diagnostics
                .iter()
                .any(|message| message.contains("renderer transfer/layout intersection"))
        );
    }

    /// Проверяет, что renderer-incompatible P010 не попадает в advertised decode matrix.
    #[test]
    fn scanner_hides_renderer_incompatible_surface_formats() {
        let mut scanner = CapabilityScanner::new();
        scanner.register_provider(Box::new(StaticVideoProvider {
            capabilities: backend_with_outputs(vec![
                output(
                    vp9_format(
                        Vp9Profile::Profile0,
                        BitDepth::Eight,
                        ChromaSubsampling::Yuv420,
                        false,
                    ),
                    VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
                ),
                output(
                    vp9_format(
                        Vp9Profile::Profile2,
                        BitDepth::Ten,
                        ChromaSubsampling::Yuv420,
                        true,
                    ),
                    VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
                ),
            ]),
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
            capabilities: backend_with_outputs(vec![output(
                vp9_format(
                    Vp9Profile::Profile0,
                    BitDepth::Eight,
                    ChromaSubsampling::Yuv420,
                    false,
                ),
                VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
            )]),
        }));
        scanner.register_render_capabilities(RenderCapabilities::wgpu_nv12(Some(4096)));

        let report = scanner.scan_with_timestamp(7);

        assert_eq!(report.supported_video_outputs().count(), 1);
        assert!(report.video_backends[0].diagnostics.is_empty());
    }

    /// Проверяет, что software output playable только при exact host-upload contract renderer-а.
    #[test]
    fn scanner_intersects_software_outputs_only_for_matching_host_upload_contract() {
        let software_backend_id =
            DecodeBackendId::new("ffmpeg-sw").expect("test backend id is valid");
        let software_output = output_for_backend(
            software_backend_id.clone(),
            vp9_format(
                Vp9Profile::Profile1,
                BitDepth::Eight,
                ChromaSubsampling::Yuv422,
                false,
            ),
            VideoFrameContract {
                pixel_layout: VideoFramePixelLayout::Yuv422Planar8,
                transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
            },
        );
        let software_backend = backend_with_id_and_outputs(
            software_backend_id,
            "Test FFmpeg software",
            vec![software_output],
        );

        let mut non_matching_scanner = CapabilityScanner::new();
        non_matching_scanner.register_provider(Box::new(StaticVideoProvider {
            capabilities: software_backend.clone(),
        }));
        non_matching_scanner.register_render_capabilities(fake_renderer_with_contracts(vec![
            VideoFrameContract {
                pixel_layout: VideoFramePixelLayout::Yuv420Planar8,
                transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
            },
        ]));

        let non_matching_report = non_matching_scanner.scan_with_timestamp(7);

        assert_eq!(non_matching_report.raw_video_outputs().count(), 1);
        assert_eq!(non_matching_report.supported_video_outputs().count(), 0);

        let mut matching_scanner = CapabilityScanner::new();
        matching_scanner.register_provider(Box::new(StaticVideoProvider {
            capabilities: software_backend,
        }));
        matching_scanner.register_render_capabilities(fake_renderer_with_contracts(vec![
            VideoFrameContract {
                pixel_layout: VideoFramePixelLayout::Yuv422Planar8,
                transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
            },
        ]));

        let matching_report = matching_scanner.scan_with_timestamp(7);

        assert_eq!(matching_report.raw_video_outputs().count(), 1);
        assert_eq!(matching_report.supported_video_outputs().count(), 1);
        assert_eq!(
            matching_report.playable_video_outputs[0]
                .frame_contract
                .pixel_layout,
            VideoFramePixelLayout::Yuv422Planar8
        );
    }
}
