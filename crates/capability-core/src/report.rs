use std::time::{SystemTime, UNIX_EPOCH};

use codec_core::{DecodeBackendId, SupportedVideoDecodeFormat};
use render_core::RenderCapabilities;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Версия JSON/report схемы capability layer.
pub type CapabilitySchemaVersion = u32;

/// Текущая версия capability report.
pub const CURRENT_CAPABILITY_SCHEMA_VERSION: CapabilitySchemaVersion = 2;

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
            let capabilities = provider.probe();
            if capabilities.backend_id != backend_id {
                warn!(
                    expected = %backend_id,
                    actual = %capabilities.backend_id,
                    "Capability provider вернул другой backend id"
                );
            }
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
            diagnostics: Vec::new(),
        }
    }

    /// Формирует одну строку report для backend-а.
    #[must_use]
    pub fn summary_text(&self) -> String {
        match &self.status {
            BackendProbeStatus::Available => format!(
                "{}: доступен, {} decode formats{}",
                self.display_name,
                self.supported_video_decode_formats.len(),
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

/// Способ передать decoded frame из decoder backend в renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoExportPath {
    /// DMA-BUF export/import path.
    DmaBuf,

    /// CPU readback/upload fallback path.
    CpuReadback,
}

impl std::fmt::Display for VideoExportPath {
    /// Печатает export path в стабильной diagnostic форме.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::DmaBuf => "DMA-BUF",
            Self::CpuReadback => "CPU readback",
        };
        formatter.write_str(label)
    }
}
