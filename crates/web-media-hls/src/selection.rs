use std::fmt;
use std::num::{NonZeroU16, NonZeroU32};

use hls_playlist_core::{ExactReference, MediaRendition};

/// Candidate-declared audio layout, не выводимый из случайного master order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HlsAudioLayoutIntent {
    /// Audio обязан находиться в выбранном variant component-е.
    Muxed,
    /// Audio обязан прийти из отдельного AUDIO rendition playlist.
    Separate(HlsAudioRenditionEvidence),
    /// Master сам доказывает muxed/separate topology, а внешний rendition выбирается строго по evidence.
    ManifestResolved(HlsAudioRenditionEvidence),
    /// Native admission доказал exact AUDIO group и единственный rendition внутри него.
    NativeGroupResolved {
        /// Exact group ID нужен semantic rematch-у и не является request material.
        group_id: Box<str>,
        /// Exact rendition evidence не допускает first-row fallback.
        evidence: HlsAudioRenditionEvidence,
    },
}

/// Exact ожидаемая track shape main media component-а после master topology resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HlsMainTrackLayoutIntent {
    /// Main component обязан содержать video и audio.
    MuxedAv,
    /// Main component обязан содержать только требуемый video track.
    VideoOnly,
    /// Main component обязан содержать только требуемый audio track.
    AudioOnly,
    /// Форму доказывает только содержимое: ровно один video track и не более одного audio.
    ///
    /// Используется для top-level media playlist без master: такой манифест не декларирует
    /// CODECS, поэтому «видео со звуком» и «видео без звука» различимы лишь после probe
    /// первого сегмента. Видео обязательно (оно задаёт seek anchor), звук опционален.
    VideoWithOptionalAudio,
    /// Форму доказывает только содержимое: либо видео с опциональным звуком (как
    /// `VideoWithOptionalAudio`), либо ровно один audio track без видео (интернет-радио).
    ///
    /// Только для top-level media playlist (A13-3): без master нет ни CODECS, ни RESOLUTION,
    /// поэтому «видео» и «только звук» различимы лишь после probe первого сегмента. Два видео,
    /// два звука и пустой набор дорожек по-прежнему отвергаются. Random-access политику live
    /// выводит из фактической формы, а не из этого intent-а (см. `live::open`).
    VideoWithOptionalAudioOrAudioOnly,
}

/// Explicit evidence для одного alternate audio rendition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HlsAudioRenditionEvidence {
    /// Exact rendition NAME, если candidate его сохранил.
    pub name: Option<Box<str>>,
    /// Exact LANGUAGE, если candidate его сохранил.
    pub language: Option<Box<str>>,
    /// Число каналов из extractor evidence без притворного знания coding identifiers.
    pub channel_count: Option<NonZeroU16>,
}

/// Strict master selection intent из выбранного normalized candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HlsVariantSelectionIntent {
    /// Exact resolution evidence.
    pub resolution: Option<(NonZeroU32, NonZeroU32)>,
    /// Exact RFC CODECS token-set evidence; token order не является quality signal.
    pub codecs: Option<Box<str>>,
    /// Muxed/separate audio contract.
    pub audio: HlsAudioLayoutIntent,
    /// Player-facing track shape выбранного normalized candidate-а.
    pub main_track_layout: HlsMainTrackLayoutIntent,
}

impl HlsVariantSelectionIntent {
    /// Проверяет обязательность хотя бы одного variant discriminator при master с несколькими rows.
    pub(crate) fn has_variant_evidence(&self) -> bool {
        self.resolution.is_some() || self.codecs.is_some()
    }
}

/// Bounded neutral descriptor; сам по себе ничего не загружает: загрузкой subtitles владеет
/// `HlsSubtitleTransport`, а descriptor лишь несёт метаданные rendition-а и его exact reference.
#[derive(Clone, PartialEq, Eq)]
pub struct HlsSubtitleRenditionDescriptor {
    group_id: Box<str>,
    name: Box<str>,
    language: Option<Box<str>>,
    characteristics: Option<Box<str>>,
    forced: bool,
    is_default: bool,
    autoselect: bool,
    reference: ExactReference,
}

impl HlsSubtitleRenditionDescriptor {
    pub(crate) fn from_rendition(rendition: &MediaRendition) -> Option<Self> {
        Some(Self {
            group_id: rendition.group_id.clone(),
            name: rendition.name.clone(),
            language: rendition.language.clone(),
            characteristics: rendition.characteristics.clone(),
            forced: rendition.forced,
            is_default: rendition.is_default,
            autoselect: rendition.autoselect,
            reference: rendition.uri.clone()?,
        })
    }

    /// Human-facing bounded NAME.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Master playlist group, к которому относится rendition.
    pub fn group_id(&self) -> &str {
        &self.group_id
    }

    /// Optional bounded language tag.
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// Optional accessibility characteristics без попытки интерпретации в S32B.
    pub fn characteristics(&self) -> Option<&str> {
        self.characteristics.as_deref()
    }

    /// Exact reference раскрывается только будущему subtitle transport owner-у.
    pub fn reference_for_future_fetch(&self) -> &str {
        self.reference.expose_for_resolution()
    }

    /// RFC FORCED marker сохраняется как descriptor metadata.
    pub const fn is_forced(&self) -> bool {
        self.forced
    }

    /// RFC `DEFAULT=YES`: провайдер предлагает rendition по умолчанию.
    pub const fn is_default(&self) -> bool {
        self.is_default
    }

    /// RFC `AUTOSELECT=YES`: плеер может выбрать rendition сам по языку пользователя.
    pub const fn is_autoselect(&self) -> bool {
        self.autoselect
    }
}

impl fmt::Debug for HlsSubtitleRenditionDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HlsSubtitleRenditionDescriptor")
            .field("group_id", &self.group_id)
            .field("name", &self.name)
            .field("language", &self.language)
            .field("characteristics", &self.characteristics)
            .field("forced", &self.forced)
            .field("default", &self.is_default)
            .field("autoselect", &self.autoselect)
            .field("reference", &self.reference)
            .finish()
    }
}
