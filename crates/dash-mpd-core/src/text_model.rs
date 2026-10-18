//! Модель текстовых (субтитровых) AdaptationSet: хранится отдельно от A/V, поэтому каталог
//! видео/аудио и его подбор по Representation никак не зависят от субтитров.
//!
//! Источники: ISO/IEC 23009-1 (AdaptationSet, Role, Label), DASH-IF IOP (текстовые треки),
//! ISO/IEC 14496-30 (TTML `stpp` и WebVTT `wvtt` в ISOBMFF).

use crate::model::{DashAddressing, DashBaseUrl};

/// Формат текстовой Representation, доказанный `mimeType` и `codecs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DashTextKind {
    /// Отдельный TTML/IMSC-документ (`application/ttml+xml`).
    SidecarTtml,
    /// Отдельный WebVTT-документ (`text/vtt`).
    SidecarWebVtt,
    /// TTML внутри ISOBMFF-сэмплов (`application/mp4`, кодек `stpp*`).
    SegmentedStpp,
    /// WebVTT внутри ISOBMFF-сэмплов (`application/mp4`, кодек `wvtt`).
    SegmentedWvtt,
}

/// Роль текстового набора (`urn:mpeg:dash:role:2011`); неизвестные значения - `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DashTextRole {
    /// `main`: основной набор.
    Main,
    /// `alternate`: альтернативный набор.
    Alternate,
    /// `supplementary`: дополнительный набор.
    Supplementary,
    /// `commentary`: комментарии.
    Commentary,
    /// `caption`: субтитры для глухих и слабослышащих (с описанием звуков).
    Caption,
    /// `subtitle`: обычные субтитры.
    Subtitle,
    /// `forced-subtitle`: только форсированные реплики (иностранная речь, надписи).
    ForcedSubtitle,
    /// `description`: описание для слабовидящих.
    Description,
    /// Любое другое значение схемы ролей.
    Other,
}

/// Текстовая Representation с эффективными (унаследованными) язык и название.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashTextRepresentation {
    /// Идентификатор Representation (для `$RepresentationID$`).
    pub id: String,
    /// Bandwidth для `$Bandwidth$`.
    pub bandwidth: Option<u64>,
    /// BCP 47 тег языка: Representation перекрывает AdaptationSet.
    pub language: Option<String>,
    /// Название из `Label`: Representation перекрывает AdaptationSet.
    pub label: Option<String>,
    /// Формат, доказанный MIME и кодеком.
    pub kind: DashTextKind,
    /// BaseURL уровня Representation.
    pub base_url: Option<DashBaseUrl>,
    /// Способ адресации ресурсов (после наследования от AdaptationSet).
    pub addressing: DashAddressing,
}

/// Текстовый AdaptationSet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashTextAdaptationSet {
    /// Идентификатор набора.
    pub id: Option<String>,
    /// Роли набора (`Role` и `Accessibility` со схемой ролей), в порядке манифеста.
    pub roles: Box<[DashTextRole]>,
    /// BaseURL уровня AdaptationSet.
    pub base_url: Option<DashBaseUrl>,
    /// Поддерживаемые Representation (хотя бы одна).
    pub representations: Box<[DashTextRepresentation]>,
}
