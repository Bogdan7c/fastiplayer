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
    CodecFamily, CodecKind, CodecMediaKind, NormalizedCodec, PreferredHeightPolicy,
    RawCodecIdentity, VideoHeight,
};

use crate::open::select_master;
use crate::{
    HlsAudioLayoutIntent, HlsAudioRenditionEvidence, HlsComponentContainerIntent,
    HlsContainerEvidence, HlsMainTrackLayoutIntent, HlsVariantSelectionIntent, HlsVodOpenError,
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
    #[error("native HLS video codec order пуст")]
    EmptyVideoCodecOrder,
    #[error("native HLS video codec order содержит audio codec")]
    NonVideoCodec,
    #[error("native HLS video codec order содержит duplicate")]
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeHlsTopology {
    Media,
    Master,
}

/// Admission отделяет safe fallback от malformed-HLS terminal errors.
#[derive(Debug, thiserror::Error)]
pub enum NativeHlsAdmissionError {
    #[error("response body не является HLS manifest")]
    StrictlyNotHls,
    #[error("HLS manifest не содержит достаточных declared selection evidence")]
    ExtractorMaterialRequired,
    #[error("live HLS остаётся на существующем extractor-owned path")]
    LiveRequiresExtractor,
    #[error("HLS manifest malformed: {0}")]
    Parse(#[source] HlsParseError),
    #[error("HLS manifest profile rejected: {0}")]
    Profile(#[source] HlsProfileError),
}

/// Единственный post-admission VOD open outcome, который безопасно возвращать extractor-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeHlsOpenFallbackReason {
    /// Selected media child оказался sliding либо EVENT playlist-ом.
    LiveOrEventPlaylist,
}

/// Не превращает malformed/unsupported HLS в fallback после уже доказанного top manifest-а.
#[must_use]
pub const fn native_hls_open_fallback_reason(
    error: &HlsVodOpenError,
) -> Option<NativeHlsOpenFallbackReason> {
    match error {
        HlsVodOpenError::Profile(HlsProfileError::NonVod | HlsProfileError::EventPlaylist) => {
            Some(NativeHlsOpenFallbackReason::LiveOrEventPlaylist)
        }
        _ => None,
    }
}

/// Строго доказывает VOD media manifest либо выбирает один master row без sibling fetch-ов.
pub fn admit_native_hls_vod(
    document_bytes: &[u8],
    effective_url: &source_core::HttpRequestTarget,
    parser_limits: HlsParserLimits,
    policy: &NativeHlsSelectionPolicy,
    expected: Option<&NativeHlsSemanticSelection>,
) -> Result<NativeHlsSemanticSelection, NativeHlsAdmissionError> {
    let playlist = parse_native_hls_top(document_bytes, effective_url, parser_limits)?;

    match playlist {
        HlsPlaylist::Media(media) => {
            if !media.end_list {
                return Err(NativeHlsAdmissionError::LiveRequiresExtractor);
            }
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

/// Проверяет native VOD profile и выбирает fresh provider-default для полного catalog discovery.
/// В отличие от legacy low-load open-а, одинаковые semantic descriptors не требуют extractor:
/// текущий root связывается exact ordinal-ом, а дальнейшие refresh-ы — semantic catalog selection-ом.
pub fn admit_native_hls_vod_catalog(
    document_bytes: &[u8],
    effective_url: &source_core::HttpRequestTarget,
    parser_limits: HlsParserLimits,
    policy: &NativeHlsSelectionPolicy,
) -> Result<NativeHlsCatalogAdmission, NativeHlsAdmissionError> {
    let playlist = parse_native_hls_top(document_bytes, effective_url, parser_limits)?;

    match playlist {
        HlsPlaylist::Media(media) => {
            if !media.end_list {
                return Err(NativeHlsAdmissionError::LiveRequiresExtractor);
            }
            Ok(NativeHlsCatalogAdmission {
                selection: native_media_selection(),
                current_master_variant_index: None,
            })
        }
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

/// Media topology имеет единственный reconstructible muxed selection intent.
fn native_media_selection() -> NativeHlsSemanticSelection {
    NativeHlsSemanticSelection {
        topology: NativeHlsTopology::Media,
        runtime_intent: HlsVariantSelectionIntent {
            resolution: None,
            codecs: None,
            audio: HlsAudioLayoutIntent::Muxed,
            main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
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
    resolution: (NonZeroU32, NonZeroU32),
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
            resolution: Some(self.resolution),
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
    let (width, height) = variant.resolution?;
    let resolution = (NonZeroU32::new(width)?, NonZeroU32::new(height)?);
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
    if !has_audio_codec {
        return None;
    }
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
            let [rendition] = renditions.as_slice() else {
                return None;
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
        resolution,
        codecs,
        audio,
        main_track_layout,
        bandwidth: variant.average_bandwidth.unwrap_or(variant.bandwidth),
        video_range: variant.video_range,
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
        .then_with(|| {
            policy.preferred_height.compare(
                VideoHeight::new(left.resolution.1.get()).ok(),
                VideoHeight::new(right.resolution.1.get()).ok(),
            )
        })
        .then_with(|| right.bandwidth.cmp(&left.bandwidth))
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
mod tests {
    use super::*;
    use source_core::HttpRequestTarget;
    use web_media_core::{PreferredVideoHeight, RawCodecIdentity};

    fn target() -> HttpRequestTarget {
        HttpRequestTarget::parse_exact("https://media.example.test/master.m3u8")
            .expect("valid target")
    }

    fn policy(height: Option<u32>) -> NativeHlsSelectionPolicy {
        let height = height.map_or(PreferredHeightPolicy::NoPreference, |height| {
            PreferredHeightPolicy::Prefer(
                PreferredVideoHeight::new(height).expect("valid preferred height"),
            )
        });
        NativeHlsSelectionPolicy::new(
            height,
            vec![CodecFamily::H264, CodecFamily::Vp9, CodecFamily::Av1],
        )
        .expect("valid policy")
    }

    fn admit(
        manifest: &str,
        policy: &NativeHlsSelectionPolicy,
        expected: Option<&NativeHlsSemanticSelection>,
    ) -> Result<NativeHlsSemanticSelection, NativeHlsAdmissionError> {
        admit_native_hls_vod(
            manifest.as_bytes(),
            &target(),
            HlsParserLimits::default(),
            policy,
            expected,
        )
    }

    #[test]
    fn x36_like_master_uses_codec_then_preferred_height_without_sibling_probe() {
        let manifest = "#EXTM3U\n\
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720,CODECS=\"avc1.64001f,mp4a.40.2\"\n\
720.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,mp4a.40.2\"\n\
1080.m3u8\n";
        let selected = admit(manifest, &policy(Some(720)), None).expect("master admitted");
        assert_eq!(
            selected.runtime_intent().resolution,
            Some((
                NonZeroU32::new(1280).unwrap(),
                NonZeroU32::new(720).unwrap()
            ))
        );
    }

    #[test]
    fn exact_reopen_fails_closed_when_semantic_row_disappears() {
        let initial = "#EXTM3U\n\
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720,CODECS=\"avc1.64001f,mp4a.40.2\"\n\
720.m3u8\n";
        let changed = "#EXTM3U\n\
#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,mp4a.40.2\"\n\
1080.m3u8\n";
        let selected = admit(initial, &policy(None), None).expect("initial admitted");
        assert!(matches!(
            admit(changed, &policy(None), Some(&selected)),
            Err(NativeHlsAdmissionError::ExtractorMaterialRequired)
        ));
    }

    #[test]
    fn catalog_admission_keeps_valid_ambiguous_master_native_with_fresh_exact_index() {
        let manifest = "#EXTM3U\n\
#EXT-X-STREAM-INF:BANDWIDTH=100000,RESOLUTION=16x16,CODECS=\"avc1.42c00a,mp4a.40.2\"\n\
ts.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=200000,RESOLUTION=16x16,CODECS=\"avc1.42c00a,mp4a.40.2\"\n\
fmp4.m3u8\n";
        assert!(matches!(
            admit(manifest, &policy(None), None),
            Err(NativeHlsAdmissionError::ExtractorMaterialRequired)
        ));

        let admitted = admit_native_hls_vod_catalog(
            manifest.as_bytes(),
            &target(),
            HlsParserLimits::default(),
            &policy(None),
        )
        .expect("full catalog path не должен делегировать валидный master extractor-у");

        assert_eq!(admitted.current_master_variant_index(), Some(1));
        assert_eq!(
            admitted.runtime_intent().main_track_layout,
            HlsMainTrackLayoutIntent::MuxedAv
        );
    }

    #[test]
    fn catalog_admission_keeps_media_vod_indexless_and_live_typed() {
        let vod = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nsegment.ts\n#EXT-X-ENDLIST\n";
        let admitted = admit_native_hls_vod_catalog(
            vod.as_bytes(),
            &target(),
            HlsParserLimits::default(),
            &policy(None),
        )
        .expect("media VOD должен остаться native без fake master ordinal-а");
        assert_eq!(admitted.current_master_variant_index(), None);
        assert_eq!(
            admitted.runtime_intent().main_track_layout,
            HlsMainTrackLayoutIntent::MuxedAv
        );

        let live = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nsegment.ts\n";
        assert!(matches!(
            admit_native_hls_vod_catalog(
                live.as_bytes(),
                &target(),
                HlsParserLimits::default(),
                &policy(None),
            ),
            Err(NativeHlsAdmissionError::LiveRequiresExtractor)
        ));
    }

    #[test]
    fn separate_audio_identity_includes_exact_group_and_rendition() {
        let manifest = "#EXTM3U\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"stereo\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,URI=\"audio.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720,CODECS=\"avc1.64001f,mp4a.40.2\",AUDIO=\"stereo\"\n\
video.m3u8\n";
        let selected = admit(manifest, &policy(None), None).expect("master admitted");
        assert!(matches!(
            selected.runtime_intent().audio,
            HlsAudioLayoutIntent::NativeGroupResolved { ref group_id, ref evidence }
                if group_id.as_ref() == "stereo"
                    && evidence.name.as_deref() == Some("English")
                    && evidence.language.as_deref() == Some("en")
        ));
        assert_eq!(
            selected.runtime_intent().main_track_layout,
            HlsMainTrackLayoutIntent::VideoOnly
        );
    }

    #[test]
    fn missing_master_evidence_and_live_media_require_typed_fallback() {
        let missing = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nchild.m3u8\n";
        assert!(matches!(
            admit(missing, &policy(None), None),
            Err(NativeHlsAdmissionError::ExtractorMaterialRequired)
        ));

        let live = "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\nseg.ts\n";
        assert!(matches!(
            admit(live, &policy(None), None),
            Err(NativeHlsAdmissionError::LiveRequiresExtractor)
        ));
    }

    #[test]
    fn non_hls_and_malformed_hls_remain_distinct() {
        assert!(matches!(
            admit("<html>login</html>", &policy(None), None),
            Err(NativeHlsAdmissionError::StrictlyNotHls)
        ));
        assert!(matches!(
            admit("#EXTM3U\n#EXT-X-TARGETDURATION:nope\n", &policy(None), None),
            Err(NativeHlsAdmissionError::Parse(_))
        ));
    }

    #[test]
    fn only_live_profile_open_error_allows_post_admission_fallback() {
        assert_eq!(
            native_hls_open_fallback_reason(&HlsVodOpenError::Profile(HlsProfileError::NonVod,)),
            Some(NativeHlsOpenFallbackReason::LiveOrEventPlaylist)
        );
        assert_eq!(
            native_hls_open_fallback_reason(&HlsVodOpenError::Profile(
                HlsProfileError::UnsupportedEncryptionMethod,
            )),
            None
        );
        assert_eq!(
            native_hls_open_fallback_reason(&HlsVodOpenError::MissingVariant),
            None
        );
    }

    #[test]
    fn test_codec_parser_fixture_stays_known_h264() {
        let codec = NormalizedCodec::parse(
            RawCodecIdentity::new("avc1.640028").expect("valid codec identity"),
        );
        assert_eq!(codec.kind(), CodecKind::Known(CodecFamily::H264));
    }

    #[test]
    fn hdr_policy_never_silently_changes_declared_dynamic_range() {
        let manifest = "#EXTM3U\n\
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,mp4a.40.2\",VIDEO-RANGE=SDR\n\
sdr.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,mp4a.40.2\",VIDEO-RANGE=PQ\n\
hdr.m3u8\n";
        assert!(matches!(
            admit(manifest, &policy(None), None),
            Err(NativeHlsAdmissionError::ExtractorMaterialRequired)
        ));

        let hdr_policy = policy(None)
            .with_dynamic_range_policy(NativeHlsDynamicRangePolicy::PreferHdrWhenAvailable);
        assert!(matches!(
            admit(manifest, &hdr_policy, None),
            Err(NativeHlsAdmissionError::ExtractorMaterialRequired)
        ));
    }
}
