//! Чистый admission direct HLS manifest-а без extractor candidate vocabulary.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::num::{NonZeroU16, NonZeroU32};

use hls_playlist_core::{
    HlsParseError, HlsParseRequest, HlsParserLimits, HlsPlaylist, HlsProfileError, HlsVideoRange,
    MasterPlaylist, MediaRendition, MediaRenditionType, VariantStream, parse_hls_playlist,
    validate_initial_profile,
};
use web_media_core::{
    CodecFamily, CodecKind, CodecMediaKind, DefaultAudioSelectionTier, NormalizedCodec,
    PreferredHeightPolicy, RawCodecIdentity, VideoHeight,
};

use crate::open::select_master;
use crate::{
    HlsAudioLayoutIntent, HlsAudioRenditionEvidence, HlsCatalogPresentation,
    HlsComponentContainerIntent, HlsContainerEvidence, HlsMainTrackLayoutIntent,
    HlsVariantSelectionIntent,
};

/// Low-load policy выбора только по authoritative master attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHlsSelectionPolicy {
    preferred_height: PreferredHeightPolicy,
    preferred_video_codecs: Box<[CodecFamily]>,
    dynamic_range: NativeHlsDynamicRangePolicy,
}

impl NativeHlsSelectionPolicy {
    /// Проверяет duplicate-free video codec order из committed user config.
    pub fn new(
        preferred_height: PreferredHeightPolicy,
        preferred_video_codecs: Vec<CodecFamily>,
    ) -> Result<Self, NativeHlsSelectionPolicyError> {
        if preferred_video_codecs.is_empty() {
            return Err(NativeHlsSelectionPolicyError::EmptyVideoCodecOrder);
        }
        if preferred_video_codecs
            .iter()
            .any(|codec| codec.media_kind() != CodecMediaKind::Video)
        {
            return Err(NativeHlsSelectionPolicyError::NonVideoCodec);
        }
        let unique = preferred_video_codecs
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        if unique.len() != preferred_video_codecs.len() {
            return Err(NativeHlsSelectionPolicyError::DuplicateVideoCodec);
        }
        Ok(Self {
            preferred_height,
            preferred_video_codecs: preferred_video_codecs.into_boxed_slice(),
            dynamic_range: NativeHlsDynamicRangePolicy::SdrOnly,
        })
    }

    /// Применяет существующую user-visible HDR policy без positional bool-а.
    #[must_use]
    pub const fn with_dynamic_range_policy(
        mut self,
        dynamic_range: NativeHlsDynamicRangePolicy,
    ) -> Self {
        self.dynamic_range = dynamic_range;
        self
    }

    fn codec_rank(&self, codec: CodecFamily) -> Option<usize> {
        self.preferred_video_codecs
            .iter()
            .position(|preferred| *preferred == codec)
    }
}

/// Native master ordering повторяет committed SDR/HDR intent без угадывания color metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeHlsDynamicRangePolicy {
    /// Declared HLG/PQ rows не допускаются; missing VIDEO-RANGE остаётся deferred evidence.
    SdrOnly,
    /// Declared HLG/PQ rows сильнее SDR/undeclared fallback bucket-а.
    PreferHdrWhenAvailable,
}

/// Invalid committed policy нельзя маскировать extractor fallback-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NativeHlsSelectionPolicyError {
    #[error("native HLS video codec order is empty")]
    EmptyVideoCodecOrder,
    #[error("native HLS video codec order contains an audio codec")]
    NonVideoCodec,
    #[error("The native HLS video codec order contains duplicate codec.")]
    DuplicateVideoCodec,
}

/// Reconstructible semantic selection без child URL или ordinal master index-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHlsSemanticSelection {
    topology: NativeHlsTopology,
    runtime_intent: HlsVariantSelectionIntent,
}

impl NativeHlsSemanticSelection {
    /// Возвращает exact runtime intent для уже rematch-нутого top manifest-а.
    #[must_use]
    pub fn runtime_intent(&self) -> HlsVariantSelectionIntent {
        self.runtime_intent.clone()
    }

    /// Container остаётся content-probed: URI/MAP/extension не являются доказательством.
    #[must_use]
    pub fn container_intent(&self) -> HlsComponentContainerIntent {
        HlsComponentContainerIntent {
            main: HlsContainerEvidence::ContentProbe,
            alternate_audio: matches!(
                self.runtime_intent.main_track_layout,
                HlsMainTrackLayoutIntent::VideoOnly
            )
            .then_some(HlsContainerEvidence::ContentProbe),
        }
    }
}

/// Fresh catalog admission дополняет semantic intent точным индексом только текущего master-а.
/// Индекс никогда не переживает root refresh: reopen использует component semantic rematch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHlsCatalogAdmission {
    selection: NativeHlsSemanticSelection,
    current_master_variant_index: Option<usize>,
    presentation_evidence: NativeHlsPresentationEvidence,
}

impl NativeHlsCatalogAdmission {
    /// Возвращает provisional runtime intent до exact catalog reopen-а.
    #[must_use]
    pub fn runtime_intent(&self) -> HlsVariantSelectionIntent {
        self.selection.runtime_intent()
    }

    /// Container остаётся content-probed на фактическом child payload-е.
    #[must_use]
    pub fn container_intent(&self) -> HlsComponentContainerIntent {
        self.selection.container_intent()
    }

    /// Exact ordinal допустим только внутри уже parsed fresh root snapshot-а.
    #[must_use]
    pub const fn current_master_variant_index(&self) -> Option<usize> {
        self.current_master_variant_index
    }

    /// Top media manifest доказывает presentation сразу; master требует ровно один child probe.
    #[must_use]
    pub const fn presentation_evidence(&self) -> NativeHlsPresentationEvidence {
        self.presentation_evidence
    }
}

/// Источник authoritative VOD/live решения без эвристик по URL или extractor metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeHlsPresentationEvidence {
    /// Top-level media manifest сам содержит ENDLIST либо live/event semantics.
    TopMedia(HlsCatalogPresentation),
    /// Master не объявляет presentation; selected media child должен быть content-probed.
    SelectedMasterChild,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeHlsTopology {
    Media,
    Master,
}

/// Admission отделяет safe fallback от malformed-HLS terminal errors.
#[derive(Debug, thiserror::Error)]
pub enum NativeHlsAdmissionError {
    #[error("response body is not HLS manifest")]
    StrictlyNotHls,
    #[error("HLS manifest does not contain sufficient declared selection evidence")]
    ExtractorMaterialRequired,
    #[error("HLS manifest malformed: {0}")]
    Parse(#[source] HlsParseError),
    #[error("HLS manifest profile rejected: {0}")]
    Profile(#[source] HlsProfileError),
}

/// Допускает media manifest либо выбирает один master row без sibling fetch-ов.
///
/// Presentation определяется отдельным typed evidence в полном catalog admission; этот
/// low-load helper намеренно не превращает отсутствие `EXT-X-ENDLIST` в extractor fallback.
pub fn admit_native_hls(
    document_bytes: &[u8],
    effective_url: &source_core::HttpRequestTarget,
    parser_limits: HlsParserLimits,
    policy: &NativeHlsSelectionPolicy,
    expected: Option<&NativeHlsSemanticSelection>,
) -> Result<NativeHlsSemanticSelection, NativeHlsAdmissionError> {
    let playlist = parse_native_hls_top(document_bytes, effective_url, parser_limits)?;

    match playlist {
        HlsPlaylist::Media(_) => {
            if expected.is_some_and(|selection| selection.topology != NativeHlsTopology::Media) {
                return Err(NativeHlsAdmissionError::ExtractorMaterialRequired);
            }
            Ok(native_media_selection())
        }
        HlsPlaylist::Master(master) => match expected {
            Some(expected) => rematch_master(&master, expected),
            None => select_master_low_load(&master, policy),
        },
    }
}

/// Проверяет native HLS top profile и выбирает fresh provider-default для полного catalog discovery.
/// В отличие от legacy low-load open-а, одинаковые semantic descriptors не требуют extractor:
/// текущий root связывается exact ordinal-ом, а дальнейшие refresh-ы — semantic catalog selection-ом.
pub fn admit_native_hls_catalog(
    document_bytes: &[u8],
    effective_url: &source_core::HttpRequestTarget,
    parser_limits: HlsParserLimits,
    policy: &NativeHlsSelectionPolicy,
) -> Result<NativeHlsCatalogAdmission, NativeHlsAdmissionError> {
    let playlist = parse_native_hls_top(document_bytes, effective_url, parser_limits)?;

    match playlist {
        HlsPlaylist::Media(media) => Ok(NativeHlsCatalogAdmission {
            selection: native_media_selection(),
            current_master_variant_index: None,
            presentation_evidence: NativeHlsPresentationEvidence::TopMedia(if media.end_list {
                HlsCatalogPresentation::Vod
            } else {
                HlsCatalogPresentation::Live
            }),
        }),
        HlsPlaylist::Master(master) => {
            let mut candidates = master
                .variants
                .iter()
                .enumerate()
                .filter_map(|(variant_index, variant)| {
                    native_candidate(&master, variant_index, variant, policy)
                })
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                return Err(NativeHlsAdmissionError::ExtractorMaterialRequired);
            }
            candidates.sort_by(|left, right| compare_candidates(left, right, policy));
            let selected = &candidates[0];
            Ok(NativeHlsCatalogAdmission {
                selection: NativeHlsSemanticSelection {
                    topology: NativeHlsTopology::Master,
                    runtime_intent: selected.runtime_intent(),
                },
                current_master_variant_index: Some(selected.variant_index),
                presentation_evidence: NativeHlsPresentationEvidence::SelectedMasterChild,
            })
        }
    }
}

/// Парсит и валидирует authoritative top manifest одинаково для legacy и catalog admission-а.
fn parse_native_hls_top(
    document_bytes: &[u8],
    effective_url: &source_core::HttpRequestTarget,
    parser_limits: HlsParserLimits,
) -> Result<HlsPlaylist, NativeHlsAdmissionError> {
    if !looks_like_hls(document_bytes) {
        return Err(NativeHlsAdmissionError::StrictlyNotHls);
    }
    let playlist = parse_hls_playlist(HlsParseRequest {
        document_bytes,
        reference_base: Some(effective_url.expose_secret_for_request()),
        limits: parser_limits,
    })
    .map_err(NativeHlsAdmissionError::Parse)?;
    validate_initial_profile(&playlist).map_err(NativeHlsAdmissionError::Profile)?;
    Ok(playlist)
}

/// Media topology имеет единственный reconstructible selection intent.
///
/// Top-level media playlist не декларирует CODECS, поэтому звук в нём не гарантирован
/// (видео без звука - обычный случай, A13-2; радио без видео - A13-3): форма доказывается
/// содержимым первого сегмента, а не предположением «видео + звук».
fn native_media_selection() -> NativeHlsSemanticSelection {
    NativeHlsSemanticSelection {
        topology: NativeHlsTopology::Media,
        runtime_intent: HlsVariantSelectionIntent {
            resolution: None,
            codecs: None,
            audio: HlsAudioLayoutIntent::Muxed,
            main_track_layout: HlsMainTrackLayoutIntent::VideoWithOptionalAudioOrAudioOnly,
        },
    }
}

fn looks_like_hls(document_bytes: &[u8]) -> bool {
    document_bytes.starts_with(b"#EXTM3U")
}

fn rematch_master(
    master: &MasterPlaylist,
    expected: &NativeHlsSemanticSelection,
) -> Result<NativeHlsSemanticSelection, NativeHlsAdmissionError> {
    if expected.topology != NativeHlsTopology::Master {
        return Err(NativeHlsAdmissionError::ExtractorMaterialRequired);
    }
    select_master(master, &expected.runtime_intent)
        .map_err(|_| NativeHlsAdmissionError::ExtractorMaterialRequired)?;
    Ok(expected.clone())
}

fn select_master_low_load(
    master: &MasterPlaylist,
    policy: &NativeHlsSelectionPolicy,
) -> Result<NativeHlsSemanticSelection, NativeHlsAdmissionError> {
    let mut candidates = master
        .variants
        .iter()
        .enumerate()
        .filter_map(|(variant_index, variant)| {
            native_candidate(master, variant_index, variant, policy)
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err(NativeHlsAdmissionError::ExtractorMaterialRequired);
    }
    candidates.sort_by(|left, right| compare_candidates(left, right, policy));
    if candidates.get(1).is_some_and(|runner_up| {
        compare_candidates(&candidates[0], runner_up, policy) == Ordering::Equal
    }) {
        return Err(NativeHlsAdmissionError::ExtractorMaterialRequired);
    }
    let selected = &candidates[0];
    let runtime_intent = selected.runtime_intent();
    select_master(master, &runtime_intent)
        .map_err(|_| NativeHlsAdmissionError::ExtractorMaterialRequired)?;
    Ok(NativeHlsSemanticSelection {
        topology: NativeHlsTopology::Master,
        runtime_intent,
    })
}

struct NativeMasterCandidate {
    variant_index: usize,
    codec_rank: usize,
    /// Default-уровень audio CODECS: AC-3/E-AC-3 variant уступает прежним при равном video.
    audio_tier: DefaultAudioSelectionTier,
    /// У audio-only variant-а (радио) RESOLUTION нет.
    resolution: Option<(NonZeroU32, NonZeroU32)>,
    codecs: Box<str>,
    audio: HlsAudioLayoutIntent,
    main_track_layout: HlsMainTrackLayoutIntent,
    bandwidth: u64,
    video_range: Option<HlsVideoRange>,
}

impl NativeMasterCandidate {
    /// Проецирует ranked row в reconstructible semantic runtime intent.
    fn runtime_intent(&self) -> HlsVariantSelectionIntent {
        HlsVariantSelectionIntent {
            resolution: self.resolution,
            codecs: Some(self.codecs.clone()),
            audio: self.audio.clone(),
            main_track_layout: self.main_track_layout,
        }
    }
}

fn native_candidate(
    master: &MasterPlaylist,
    variant_index: usize,
    variant: &VariantStream,
    policy: &NativeHlsSelectionPolicy,
) -> Option<NativeMasterCandidate> {
    let codecs = variant.codecs.clone()?;
    let parsed = codecs
        .split(',')
        .map(str::trim)
        .map(|raw| {
            RawCodecIdentity::new(raw.to_owned())
                .ok()
                .map(NormalizedCodec::parse)
        })
        .collect::<Option<Vec<_>>>()?;
    let has_known_video_codec = parsed.iter().any(|codec| {
        matches!(
            codec.kind(),
            CodecKind::Known(family) if family.media_kind() == CodecMediaKind::Video
        )
    });
    if !has_known_video_codec && variant.resolution.is_none() {
        return audio_only_candidate(variant_index, variant, codecs, &parsed);
    }
    let (width, height) = variant.resolution?;
    let resolution = Some((NonZeroU32::new(width)?, NonZeroU32::new(height)?));
    let video_codecs = parsed
        .iter()
        .filter_map(|codec| match codec.kind() {
            CodecKind::Known(family) if family.media_kind() == CodecMediaKind::Video => {
                Some(family)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let [video_codec] = video_codecs.as_slice() else {
        return None;
    };
    let has_audio_codec = parsed.iter().any(|codec| {
        matches!(
            codec.kind(),
            CodecKind::Known(family) if family.media_kind() == CodecMediaKind::Audio
        )
    });
    // Variant без audio-кодека и без AUDIO group - видео без звука (A13-2). Форму
    // доказывает probe дочернего playlist-а, а не CODECS, поэтому intent допускает оба исхода.
    // Variant с AUDIO group, но без audio-кодека остаётся неразрешимым без extractor-а.
    if !has_audio_codec && variant.audio_group.is_some() {
        return None;
    }
    let carries_no_audio_by_declaration = !has_audio_codec;
    let codec_rank = policy.codec_rank(*video_codec)?;
    if policy.dynamic_range == NativeHlsDynamicRangePolicy::SdrOnly
        && matches!(
            variant.video_range,
            Some(HlsVideoRange::Hlg | HlsVideoRange::Pq)
        )
    {
        return None;
    }
    let (audio, main_track_layout) = match variant.audio_group.as_deref() {
        None if carries_no_audio_by_declaration => (
            HlsAudioLayoutIntent::Muxed,
            HlsMainTrackLayoutIntent::VideoWithOptionalAudio,
        ),
        None => (
            HlsAudioLayoutIntent::Muxed,
            HlsMainTrackLayoutIntent::MuxedAv,
        ),
        Some(group_id) => {
            let renditions = master
                .renditions
                .iter()
                .filter(|rendition| {
                    rendition.rendition_type == MediaRenditionType::Audio
                        && rendition.group_id.as_ref() == group_id
                })
                .collect::<Vec<_>>();
            let rendition = match renditions.as_slice() {
                [only] => *only,
                alternatives => {
                    let mut defaults = alternatives
                        .iter()
                        .copied()
                        .filter(|rendition| rendition.is_default);
                    let selected = defaults.next()?;
                    if defaults.next().is_some() {
                        return None;
                    }
                    selected
                }
            };
            let evidence = rendition_evidence(rendition)?;
            (
                HlsAudioLayoutIntent::NativeGroupResolved {
                    group_id: group_id.into(),
                    evidence,
                },
                if rendition.uri.is_some() {
                    HlsMainTrackLayoutIntent::VideoOnly
                } else {
                    HlsMainTrackLayoutIntent::MuxedAv
                },
            )
        }
    };
    Some(NativeMasterCandidate {
        variant_index,
        codec_rank,
        audio_tier: declared_audio_tier(&parsed),
        resolution,
        codecs,
        audio,
        main_track_layout,
        bandwidth: variant.average_bandwidth.unwrap_or(variant.bandwidth),
        video_range: variant.video_range,
    })
}

/// Variant интернет-радио: CODECS только аудио (mp4a/…), RESOLUTION и AUDIO group нет (A13-3).
///
/// Такой variant становится кандидатом наравне с видео-variant-ами, но всегда ранжируется после
/// них (`usize::MAX` по codec rank): смешанный master (видео-лесенка + audio-only запасной
/// вариант) по-прежнему выбирает видео. Среди audio-only побеждает больший bandwidth.
fn audio_only_candidate(
    variant_index: usize,
    variant: &VariantStream,
    codecs: Box<str>,
    parsed: &[NormalizedCodec],
) -> Option<NativeMasterCandidate> {
    let audio_codec_count = parsed
        .iter()
        .filter(|codec| {
            matches!(
                codec.kind(),
                CodecKind::Known(family) if family.media_kind() == CodecMediaKind::Audio
            )
        })
        .count();
    // Ровно один известный audio-кодек и никаких неизвестных токенов: форму нельзя угадать иначе.
    if audio_codec_count != 1 || parsed.len() != 1 || variant.audio_group.is_some() {
        return None;
    }
    Some(NativeMasterCandidate {
        variant_index,
        codec_rank: usize::MAX,
        audio_tier: declared_audio_tier(parsed),
        resolution: None,
        codecs,
        audio: HlsAudioLayoutIntent::Muxed,
        main_track_layout: HlsMainTrackLayoutIntent::AudioOnly,
        bandwidth: variant.average_bandwidth.unwrap_or(variant.bandwidth),
        video_range: None,
    })
}

fn rendition_evidence(rendition: &MediaRendition) -> Option<HlsAudioRenditionEvidence> {
    let channel_count = match rendition.channel_count {
        Some(count) => Some(NonZeroU16::new(u16::try_from(count.get()).ok()?)?),
        None => None,
    };
    Some(HlsAudioRenditionEvidence {
        name: Some(rendition.name.clone()),
        language: rendition.language.clone(),
        channel_count,
    })
}

fn compare_candidates(
    left: &NativeMasterCandidate,
    right: &NativeMasterCandidate,
    policy: &NativeHlsSelectionPolicy,
) -> Ordering {
    dynamic_range_rank(left.video_range, policy.dynamic_range)
        .cmp(&dynamic_range_rank(right.video_range, policy.dynamic_range))
        .then_with(|| left.codec_rank.cmp(&right.codec_rank))
        // Больший tier лучше, поэтому right vs left. Стоит после video codec-а (выбор
        // пользователя), но до высоты/bandwidth: иначе E-AC-3 variant, ставший
        // воспроизводимым, молча вытеснил бы прежний AAC variant большим BANDWIDTH.
        .then_with(|| right.audio_tier.cmp(&left.audio_tier))
        .then_with(|| {
            policy.preferred_height.compare(
                left.resolution
                    .and_then(|(_, height)| VideoHeight::new(height.get()).ok()),
                right
                    .resolution
                    .and_then(|(_, height)| VideoHeight::new(height.get()).ok()),
            )
        })
        .then_with(|| right.bandwidth.cmp(&left.bandwidth))
}

/// Уровень variant-а по объявленным audio CODECS.
///
/// `FallbackOnly`, только если variant объявляет audio и все его audio codecs —
/// fallback-only families; variant без audio остаётся `Primary` (прежнее поведение).
fn declared_audio_tier(parsed: &[NormalizedCodec]) -> DefaultAudioSelectionTier {
    parsed
        .iter()
        .filter(|codec| {
            matches!(
                codec.kind(),
                CodecKind::Known(family) if family.media_kind() == CodecMediaKind::Audio
            )
        })
        .map(|codec| DefaultAudioSelectionTier::for_codec(codec.kind()))
        .max()
        .unwrap_or(DefaultAudioSelectionTier::Primary)
}

fn dynamic_range_rank(
    video_range: Option<HlsVideoRange>,
    policy: NativeHlsDynamicRangePolicy,
) -> u8 {
    match (policy, video_range) {
        (
            NativeHlsDynamicRangePolicy::PreferHdrWhenAvailable,
            Some(HlsVideoRange::Hlg | HlsVideoRange::Pq),
        ) => 0,
        _ => 1,
    }
}

#[cfg(test)]
mod ac3_tests;

#[cfg(test)]
mod tests;
