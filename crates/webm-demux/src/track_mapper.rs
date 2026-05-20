use std::collections::HashMap;
use std::time::Duration;

use bytes::Bytes;
use media_core::{TrackId, TrackInfo, TrackKind};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::well_known as audio_codec;
use symphonia::core::codecs::audio::{AudioCodecId, CODEC_ID_NULL_AUDIO};
use symphonia::core::codecs::subtitle::well_known as subtitle_codec;
use symphonia::core::codecs::subtitle::{CODEC_ID_NULL_SUBTITLE, SubtitleCodecId};
use symphonia::core::codecs::video::well_known as video_codec;
use symphonia::core::codecs::video::{CODEC_ID_NULL_VIDEO, VideoCodecId};
use symphonia::core::formats::{Track, TrackType};
use symphonia::core::units::TimeBase as SymphoniaTimeBase;
use tracing::warn;

use crate::matroska_metadata::MatroskaVideoTrack;
use crate::symphonia_api::{media_time_base_from_symphonia, symphonia_duration_to_duration};

/// Codec id для video track-а, когда контейнер не дал доказательства конкретного codec-а.
const UNKNOWN_VIDEO_CODEC_ID: &str = "unknown_video";

/// Codec id для audio track-а, когда контейнер не дал доказательства конкретного codec-а.
const UNKNOWN_AUDIO_CODEC_ID: &str = "unknown_audio";

/// Codec id для subtitle/unknown track-а, который не входит в neutral playback model.
const UNSUPPORTED_TRACK_CODEC_ID: &str = "unsupported_track";

/// Внутренняя запись track-а, нужная packet/seek mapper-ам после открытия demuxer-а.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrackEntry {
    pub(crate) kind: TrackEntryKind,
    pub(crate) codec_id: String,
    pub(crate) time_base: Option<SymphoniaTimeBase>,
    pub(crate) sample_rate: Option<u32>,
    pub(crate) channels: Option<u32>,
}

impl TrackEntry {
    /// Возвращает media-core kind только для треков, которые pipeline умеет маршрутизировать.
    pub(crate) const fn supported_kind(&self) -> Option<TrackKind> {
        self.kind.supported_kind()
    }

    /// Возвращает причину, по которой track оставлен только во внутренней map для packet skip-а.
    pub(crate) const fn unsupported_kind(&self) -> Option<UnsupportedTrackKind> {
        self.kind.unsupported_kind()
    }
}

/// Внутренний тип track-а: public model пока знает только audio/video, но demuxer должен помнить
/// unsupported tracks, чтобы их packets не выглядели как неизвестный track id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrackEntryKind {
    /// Track поддержан neutral media-core model и может попасть в `TrackInfo`.
    Supported(TrackKind),

    /// Track известен контейнеру, но текущая public модель его не маршрутизирует.
    Unsupported(UnsupportedTrackKind),
}

impl TrackEntryKind {
    /// Возвращает public `TrackKind`, если track поддержан.
    pub(crate) const fn supported_kind(self) -> Option<TrackKind> {
        match self {
            Self::Supported(kind) => Some(kind),
            Self::Unsupported(_) => None,
        }
    }

    /// Возвращает unsupported причину, если track не должен попасть в public track list.
    pub(crate) const fn unsupported_kind(self) -> Option<UnsupportedTrackKind> {
        match self {
            Self::Supported(_) => None,
            Self::Unsupported(kind) => Some(kind),
        }
    }
}

/// Unsupported track categories, которые Symphonia может отдать как track, но player пока не
/// декодирует и не выбирает.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnsupportedTrackKind {
    /// Subtitle track: его нельзя маскировать под video.
    Subtitle,

    /// Track без type-specific codec params или с неизвестным будущим типом.
    Unknown,
}

impl UnsupportedTrackKind {
    /// Короткая стабильная причина для logs и packet skip diagnostics.
    pub(crate) const fn diagnostic_reason(self) -> &'static str {
        match self {
            Self::Subtitle => "subtitle track is not supported by media-core TrackKind yet",
            Self::Unknown => "track type is unknown because codec params are absent or unsupported",
        }
    }
}

/// Политика single-video fallback-а для Matroska metadata, чтобы unknown/audio tracks не крали HDR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatroskaVideoFallbackPolicy {
    /// Можно брать только exact match по track id.
    ExactOnly,

    /// Можно взять единственный Matroska video track, если Symphonia track id отличается.
    ExactOrSingleVideo,
}

/// Полный результат адаптации Symphonia tracks в neutral media-core model.
pub(crate) struct TrackMapping {
    pub(crate) tracks: Vec<TrackInfo>,
    pub(crate) duration: Option<Duration>,
    pub(crate) track_map: HashMap<u32, TrackEntry>,
}

/// Собирает public track metadata и private lookup map из Symphonia track list.
pub(crate) fn map_tracks(
    symphonia_tracks: &[Track],
    video_tracks_by_track: &mut HashMap<TrackId, MatroskaVideoTrack>,
) -> TrackMapping {
    let mut tracks = Vec::new();
    let mut track_map = HashMap::new();
    let video_fallback_candidate_count = count_matroska_video_fallback_candidates(symphonia_tracks);

    for track in symphonia_tracks {
        let inferred_kind = infer_track_kind(track);
        let matroska_video_track = take_matroska_video_track_for_mapping(
            TrackId::new(track.id),
            inferred_kind,
            video_fallback_candidate_count,
            video_tracks_by_track,
        );
        let track_entry = build_track_entry(track, matroska_video_track.as_ref());
        if let Some(track_info) = build_track_info(track, &track_entry, matroska_video_track) {
            tracks.push(track_info);
        } else {
            log_unsupported_track(track, &track_entry);
        }

        track_map.insert(track.id, track_entry);
    }

    let duration = tracks.iter().filter_map(|track| track.duration).max();

    TrackMapping {
        tracks,
        duration,
        track_map,
    }
}

/// Считает только те Symphonia tracks, которые могут законно принять Matroska video fallback.
fn count_matroska_video_fallback_candidates(symphonia_tracks: &[Track]) -> usize {
    symphonia_tracks
        .iter()
        .filter(|track| {
            matches!(
                infer_track_kind(track),
                TrackEntryKind::Supported(TrackKind::Video)
                    | TrackEntryKind::Unsupported(UnsupportedTrackKind::Unknown)
            )
        })
        .count()
}

/// Возвращает `true`, если список tracks содержит video/unknown track и ему может понадобиться
/// Matroska prefix scan для HDR или codec id fallback-а.
pub(crate) fn tracks_may_need_matroska_video_metadata(symphonia_tracks: &[Track]) -> bool {
    count_matroska_video_fallback_candidates(symphonia_tracks) > 0
}

/// Достаёт Matroska video track metadata для Symphonia track id.
///
/// Symphonia может использовать внутренний `track.id`, который не равен Matroska
/// `TrackNumber`. Если pre-scan нашёл ровно один video entry, fallback безопасен:
/// двусмысленности между несколькими видеотреками нет, а HDR metadata не теряется.
fn take_matroska_video_track_for_mapping(
    symphonia_track_id: TrackId,
    inferred_kind: TrackEntryKind,
    video_fallback_candidate_count: usize,
    video_tracks_by_track: &mut HashMap<TrackId, MatroskaVideoTrack>,
) -> Option<MatroskaVideoTrack> {
    let fallback_policy =
        matroska_video_fallback_policy(inferred_kind, video_fallback_candidate_count)?;

    if let Some(video_track) = video_tracks_by_track.remove(&symphonia_track_id) {
        return Some(video_track);
    }

    if fallback_policy != MatroskaVideoFallbackPolicy::ExactOrSingleVideo
        || video_tracks_by_track.len() != 1
    {
        return None;
    }

    let matroska_track_id = video_tracks_by_track.keys().next().copied()?;
    let video_track = video_tracks_by_track.remove(&matroska_track_id);
    if video_track.is_some() {
        warn!(
            symphonia_track_id = %symphonia_track_id,
            matroska_track_id = %matroska_track_id,
            "Matroska video track metadata сопоставлена по единственному video track fallback"
        );
    }
    video_track
}

/// Выбирает Matroska fallback policy по доказательствам типа track-а.
fn matroska_video_fallback_policy(
    inferred_kind: TrackEntryKind,
    video_fallback_candidate_count: usize,
) -> Option<MatroskaVideoFallbackPolicy> {
    match inferred_kind {
        TrackEntryKind::Supported(TrackKind::Video) => {
            Some(MatroskaVideoFallbackPolicy::ExactOrSingleVideo)
        }
        TrackEntryKind::Unsupported(UnsupportedTrackKind::Unknown)
            if video_fallback_candidate_count == 1 =>
        {
            Some(MatroskaVideoFallbackPolicy::ExactOrSingleVideo)
        }
        TrackEntryKind::Unsupported(UnsupportedTrackKind::Unknown) => {
            Some(MatroskaVideoFallbackPolicy::ExactOnly)
        }
        TrackEntryKind::Supported(TrackKind::Audio)
        | TrackEntryKind::Unsupported(UnsupportedTrackKind::Subtitle) => None,
    }
}

/// Определяет тип трека только по Symphonia 0.6 track type/type-specific codec params.
pub(crate) fn infer_track_kind(track: &Track) -> TrackEntryKind {
    match track.track_type() {
        Some(TrackType::Audio) => TrackEntryKind::Supported(TrackKind::Audio),
        Some(TrackType::Video) => TrackEntryKind::Supported(TrackKind::Video),
        Some(TrackType::Subtitle) => TrackEntryKind::Unsupported(UnsupportedTrackKind::Subtitle),
        Some(_) | None => infer_track_kind_from_codec_params(track.codec_params.as_ref()),
    }
}

/// Fallback для non-exhaustive будущих `TrackType` значений: верим только typed codec params.
fn infer_track_kind_from_codec_params(codec_params: Option<&CodecParameters>) -> TrackEntryKind {
    match codec_params {
        Some(CodecParameters::Audio(_)) => TrackEntryKind::Supported(TrackKind::Audio),
        Some(CodecParameters::Video(_)) => TrackEntryKind::Supported(TrackKind::Video),
        Some(CodecParameters::Subtitle(_)) => {
            TrackEntryKind::Unsupported(UnsupportedTrackKind::Subtitle)
        }
        Some(_) | None => TrackEntryKind::Unsupported(UnsupportedTrackKind::Unknown),
    }
}

/// Определяет тип трека и codec_id из Symphonia 0.6 codec params и Matroska CodecID.
pub(crate) fn build_track_entry(
    track: &Track,
    matroska_video_track: Option<&MatroskaVideoTrack>,
) -> TrackEntry {
    let (sample_rate, channels) = audio_properties_from_codec_params(track);
    let kind = effective_track_kind(infer_track_kind(track), matroska_video_track);
    let matroska_codec_id = matroska_video_track.and_then(|video_track| {
        video_track
            .codec_id
            .as_deref()
            .and_then(normalize_matroska_codec_id)
    });
    let codec_id = resolve_track_codec_id(track.codec_params.as_ref(), kind, matroska_codec_id);

    TrackEntry {
        kind,
        codec_id,
        time_base: track.time_base,
        sample_rate,
        channels,
    }
}

/// Matroska video metadata является достаточным доказательством video только после policy check-а.
fn effective_track_kind(
    inferred_kind: TrackEntryKind,
    matroska_video_track: Option<&MatroskaVideoTrack>,
) -> TrackEntryKind {
    if matroska_video_track.is_some() {
        TrackEntryKind::Supported(TrackKind::Video)
    } else {
        inferred_kind
    }
}

/// Преобразует один Symphonia track в public media-core `TrackInfo`.
fn build_track_info(
    track: &Track,
    track_entry: &TrackEntry,
    matroska_video_track: Option<MatroskaVideoTrack>,
) -> Option<TrackInfo> {
    let kind = track_entry.supported_kind()?;
    let duration = track_entry
        .time_base
        .zip(track.duration)
        .map(|(time_base, duration)| symphonia_duration_to_duration(time_base, duration));
    let time_base = track_entry
        .time_base
        .and_then(media_time_base_from_symphonia);
    let codec_private = track
        .codec_params
        .as_ref()
        .and_then(codec_private_from_codec_params);
    let video = (kind == TrackKind::Video)
        .then(|| matroska_video_track.and_then(|video_track| video_track.metadata))
        .flatten();

    Some(TrackInfo {
        id: TrackId::new(track.id),
        kind,
        codec_id: track_entry.codec_id.clone(),
        codec_private,
        time_base,
        duration,
        sample_rate: track_entry.sample_rate,
        channels: track_entry.channels,
        video,
    })
}

/// Логирует unsupported track один раз на open, а не для каждого packet-а.
fn log_unsupported_track(track: &Track, track_entry: &TrackEntry) {
    let Some(unsupported_kind) = track_entry.unsupported_kind() else {
        return;
    };

    warn!(
        track_id = track.id,
        codec_id = %track_entry.codec_id,
        track_type = ?track.track_type(),
        reason = unsupported_kind.diagnostic_reason(),
        "Unsupported Symphonia track skipped"
    );
}

/// Достаёт codec private bytes из type-specific Symphonia 0.6 codec params.
fn codec_private_from_codec_params(codec_params: &CodecParameters) -> Option<Bytes> {
    match codec_params {
        CodecParameters::Audio(audio_params) => audio_params
            .extra_data
            .as_deref()
            .map(Bytes::copy_from_slice),
        CodecParameters::Video(video_params) => video_params
            .extra_data
            .iter()
            .find(|extra_data| !extra_data.data.is_empty())
            .map(|extra_data| Bytes::copy_from_slice(&extra_data.data)),
        CodecParameters::Subtitle(_) => None,
        _ => None,
    }
}

/// Достаёт audio sample rate/channels, включая ручной OpusHead fallback для WebM.
fn audio_properties_from_codec_params(track: &Track) -> (Option<u32>, Option<u32>) {
    let Some(CodecParameters::Audio(audio_params)) = track.codec_params.as_ref() else {
        return (None, None);
    };

    let mut sample_rate = audio_params.sample_rate;
    let mut channels = audio_params
        .channels
        .as_ref()
        .map(|channels| channels.count() as u32);

    if (sample_rate.is_none() || channels.is_none())
        && let Some(codec_private) = audio_params.extra_data.as_deref()
        && let Some((opus_sample_rate, opus_channels)) = parse_opus_head(codec_private)
    {
        sample_rate = sample_rate.or(Some(opus_sample_rate));
        channels = channels.or(Some(opus_channels));
    }

    (sample_rate, channels)
}

/// Нормализует Matroska CodecID без предположений о том, поддерживаем ли codec.
fn normalize_matroska_codec_id(codec_id: &str) -> Option<String> {
    let trimmed_codec_id = codec_id.trim();
    if trimmed_codec_id.is_empty() {
        None
    } else {
        Some(trimmed_codec_id.to_ascii_uppercase())
    }
}

/// Возвращает container codec id с приоритетом явного Matroska CodecID.
fn resolve_track_codec_id(
    codec_params: Option<&CodecParameters>,
    kind: TrackEntryKind,
    matroska_codec_id: Option<String>,
) -> String {
    if let Some(codec_id) = matroska_codec_id {
        return codec_id;
    }

    if let Some(codec_id) = codec_id_from_symphonia_codec(codec_params) {
        return codec_id.to_string();
    }

    if codec_params_has_null_codec(codec_params) {
        return unknown_codec_id_for_kind(kind).to_string();
    }

    codec_params
        .map(symphonia_codec_diagnostic_id)
        .unwrap_or_else(|| unknown_codec_id_for_kind(kind).to_string())
}

/// Таблица Symphonia 0.6 codec ids, которую можно расширять без переписывания demux policy.
fn codec_id_from_symphonia_codec(codec_params: Option<&CodecParameters>) -> Option<&'static str> {
    match codec_params {
        Some(CodecParameters::Audio(audio_params)) => {
            codec_id_from_symphonia_audio_codec(audio_params.codec)
        }
        Some(CodecParameters::Video(video_params)) => {
            codec_id_from_symphonia_video_codec(video_params.codec)
        }
        Some(CodecParameters::Subtitle(subtitle_params)) => {
            codec_id_from_symphonia_subtitle_codec(subtitle_params.codec)
        }
        Some(_) | None => None,
    }
}

/// Мапит известные audio codec ids Symphonia в container ids, ожидаемые capability layer-ом.
fn codec_id_from_symphonia_audio_codec(codec: AudioCodecId) -> Option<&'static str> {
    match codec {
        audio_codec::CODEC_ID_PCM_S32LE => Some("A_PCM_S32LE"),
        audio_codec::CODEC_ID_PCM_S32LE_PLANAR => Some("A_PCM_S32LE_PLANAR"),
        audio_codec::CODEC_ID_PCM_S32BE => Some("A_PCM_S32BE"),
        audio_codec::CODEC_ID_PCM_S32BE_PLANAR => Some("A_PCM_S32BE_PLANAR"),
        audio_codec::CODEC_ID_PCM_S24LE => Some("A_PCM_S24LE"),
        audio_codec::CODEC_ID_PCM_S24LE_PLANAR => Some("A_PCM_S24LE_PLANAR"),
        audio_codec::CODEC_ID_PCM_S24BE => Some("A_PCM_S24BE"),
        audio_codec::CODEC_ID_PCM_S24BE_PLANAR => Some("A_PCM_S24BE_PLANAR"),
        audio_codec::CODEC_ID_PCM_S16LE => Some("A_PCM_S16LE"),
        audio_codec::CODEC_ID_PCM_S16LE_PLANAR => Some("A_PCM_S16LE_PLANAR"),
        audio_codec::CODEC_ID_PCM_S16BE => Some("A_PCM_S16BE"),
        audio_codec::CODEC_ID_PCM_S16BE_PLANAR => Some("A_PCM_S16BE_PLANAR"),
        audio_codec::CODEC_ID_PCM_S8 => Some("A_PCM_S8"),
        audio_codec::CODEC_ID_PCM_S8_PLANAR => Some("A_PCM_S8_PLANAR"),
        audio_codec::CODEC_ID_PCM_U32LE => Some("A_PCM_U32LE"),
        audio_codec::CODEC_ID_PCM_U32LE_PLANAR => Some("A_PCM_U32LE_PLANAR"),
        audio_codec::CODEC_ID_PCM_U32BE => Some("A_PCM_U32BE"),
        audio_codec::CODEC_ID_PCM_U32BE_PLANAR => Some("A_PCM_U32BE_PLANAR"),
        audio_codec::CODEC_ID_PCM_U24LE => Some("A_PCM_U24LE"),
        audio_codec::CODEC_ID_PCM_U24LE_PLANAR => Some("A_PCM_U24LE_PLANAR"),
        audio_codec::CODEC_ID_PCM_U24BE => Some("A_PCM_U24BE"),
        audio_codec::CODEC_ID_PCM_U24BE_PLANAR => Some("A_PCM_U24BE_PLANAR"),
        audio_codec::CODEC_ID_PCM_U16LE => Some("A_PCM_U16LE"),
        audio_codec::CODEC_ID_PCM_U16LE_PLANAR => Some("A_PCM_U16LE_PLANAR"),
        audio_codec::CODEC_ID_PCM_U16BE => Some("A_PCM_U16BE"),
        audio_codec::CODEC_ID_PCM_U16BE_PLANAR => Some("A_PCM_U16BE_PLANAR"),
        audio_codec::CODEC_ID_PCM_U8 => Some("A_PCM_U8"),
        audio_codec::CODEC_ID_PCM_U8_PLANAR => Some("A_PCM_U8_PLANAR"),
        audio_codec::CODEC_ID_PCM_F32LE => Some("A_PCM_F32LE"),
        audio_codec::CODEC_ID_PCM_F32LE_PLANAR => Some("A_PCM_F32LE_PLANAR"),
        audio_codec::CODEC_ID_PCM_F32BE => Some("A_PCM_F32BE"),
        audio_codec::CODEC_ID_PCM_F32BE_PLANAR => Some("A_PCM_F32BE_PLANAR"),
        audio_codec::CODEC_ID_PCM_F64LE => Some("A_PCM_F64LE"),
        audio_codec::CODEC_ID_PCM_F64LE_PLANAR => Some("A_PCM_F64LE_PLANAR"),
        audio_codec::CODEC_ID_PCM_F64BE => Some("A_PCM_F64BE"),
        audio_codec::CODEC_ID_PCM_F64BE_PLANAR => Some("A_PCM_F64BE_PLANAR"),
        audio_codec::CODEC_ID_PCM_ALAW => Some("A_PCM_ALAW"),
        audio_codec::CODEC_ID_PCM_MULAW => Some("A_PCM_MULAW"),
        audio_codec::CODEC_ID_ADPCM_G722 => Some("A_ADPCM_G722"),
        audio_codec::CODEC_ID_ADPCM_G726 => Some("A_ADPCM_G726"),
        audio_codec::CODEC_ID_ADPCM_G726LE => Some("A_ADPCM_G726LE"),
        audio_codec::CODEC_ID_ADPCM_MS => Some("A_ADPCM_MS"),
        audio_codec::CODEC_ID_ADPCM_IMA_WAV => Some("A_ADPCM_IMA_WAV"),
        audio_codec::CODEC_ID_ADPCM_IMA_QT => Some("A_ADPCM_IMA_QT"),
        audio_codec::CODEC_ID_OPUS => Some("A_OPUS"),
        audio_codec::CODEC_ID_VORBIS => Some("A_VORBIS"),
        audio_codec::CODEC_ID_SPEEX => Some("A_SPEEX"),
        audio_codec::CODEC_ID_MUSEPACK => Some("A_MUSEPACK"),
        audio_codec::CODEC_ID_MP1 => Some("A_MP1"),
        audio_codec::CODEC_ID_MP2 => Some("A_MP2"),
        audio_codec::CODEC_ID_MP3 => Some("A_MP3"),
        audio_codec::CODEC_ID_AAC => Some("A_AAC"),
        audio_codec::CODEC_ID_AC3 => Some("A_AC3"),
        audio_codec::CODEC_ID_EAC3 => Some("A_EAC3"),
        audio_codec::CODEC_ID_AC4 => Some("A_AC4"),
        audio_codec::CODEC_ID_DCA => Some("A_DCA"),
        audio_codec::CODEC_ID_ATRAC1 => Some("A_ATRAC1"),
        audio_codec::CODEC_ID_ATRAC3 => Some("A_ATRAC3"),
        audio_codec::CODEC_ID_ATRAC3PLUS => Some("A_ATRAC3PLUS"),
        audio_codec::CODEC_ID_ATRAC9 => Some("A_ATRAC9"),
        audio_codec::CODEC_ID_WMA => Some("A_WMA"),
        audio_codec::CODEC_ID_RA10 => Some("A_RA10"),
        audio_codec::CODEC_ID_RA20 => Some("A_RA20"),
        audio_codec::CODEC_ID_SIPR => Some("A_SIPR"),
        audio_codec::CODEC_ID_COOK => Some("A_COOK"),
        audio_codec::CODEC_ID_SBC => Some("A_SBC"),
        audio_codec::CODEC_ID_APTX => Some("A_APTX"),
        audio_codec::CODEC_ID_APTX_HD => Some("A_APTX_HD"),
        audio_codec::CODEC_ID_LDAC => Some("A_LDAC"),
        audio_codec::CODEC_ID_BINK_AUDIO => Some("A_BINK_AUDIO"),
        audio_codec::CODEC_ID_SMACKER_AUDIO => Some("A_SMACKER_AUDIO"),
        audio_codec::CODEC_ID_FLAC => Some("A_FLAC"),
        audio_codec::CODEC_ID_WAVPACK => Some("A_WAVPACK"),
        audio_codec::CODEC_ID_MONKEYS_AUDIO => Some("A_MONKEYS_AUDIO"),
        audio_codec::CODEC_ID_ALAC => Some("A_ALAC"),
        audio_codec::CODEC_ID_TTA => Some("A_TTA"),
        audio_codec::CODEC_ID_RALF => Some("A_RALF"),
        audio_codec::CODEC_ID_TRUEHD => Some("A_TRUEHD"),
        _ => None,
    }
}

/// Мапит известные video codec ids Symphonia в container ids, ожидаемые capability layer-ом.
fn codec_id_from_symphonia_video_codec(codec: VideoCodecId) -> Option<&'static str> {
    match codec {
        video_codec::CODEC_ID_VP8 => Some("V_VP8"),
        video_codec::CODEC_ID_VP9 => Some("V_VP9"),
        video_codec::CODEC_ID_AV1 => Some("V_AV1"),
        video_codec::CODEC_ID_H264 => Some("V_MPEG4/ISO/AVC"),
        video_codec::CODEC_ID_HEVC => Some("V_MPEGH/ISO/HEVC"),
        _ => None,
    }
}

/// Мапит subtitle codec ids в стабильные diagnostic ids; public TrackKind пока их не содержит.
fn codec_id_from_symphonia_subtitle_codec(codec: SubtitleCodecId) -> Option<&'static str> {
    match codec {
        subtitle_codec::CODEC_ID_TEXT_UTF8 => Some("S_TEXT/UTF8"),
        subtitle_codec::CODEC_ID_SSA => Some("S_TEXT/SSA"),
        subtitle_codec::CODEC_ID_ASS => Some("S_TEXT/ASS"),
        subtitle_codec::CODEC_ID_SAMI => Some("S_TEXT/SAMI"),
        subtitle_codec::CODEC_ID_SRT => Some("S_TEXT/SRT"),
        subtitle_codec::CODEC_ID_WEBVTT => Some("S_TEXT/WEBVTT"),
        subtitle_codec::CODEC_ID_DVBSUB => Some("S_DVBSUB"),
        subtitle_codec::CODEC_ID_HDMV_TEXTST => Some("S_HDMV/TEXTST"),
        subtitle_codec::CODEC_ID_MOV_TEXT => Some("S_MOV_TEXT"),
        subtitle_codec::CODEC_ID_BMP => Some("S_IMAGE/BMP"),
        subtitle_codec::CODEC_ID_VOBSUB => Some("S_VOBSUB"),
        subtitle_codec::CODEC_ID_HDMV_PGS => Some("S_HDMV/PGS"),
        subtitle_codec::CODEC_ID_KATE => Some("S_KATE"),
        _ => None,
    }
}

/// Проверяет, что Symphonia явно не знает codec.
fn codec_params_has_null_codec(codec_params: Option<&CodecParameters>) -> bool {
    match codec_params {
        Some(CodecParameters::Audio(audio_params)) => audio_params.codec == CODEC_ID_NULL_AUDIO,
        Some(CodecParameters::Video(video_params)) => video_params.codec == CODEC_ID_NULL_VIDEO,
        Some(CodecParameters::Subtitle(subtitle_params)) => {
            subtitle_params.codec == CODEC_ID_NULL_SUBTITLE
        }
        Some(_) | None => true,
    }
}

/// Возвращает стабильный diagnostic id для известного Symphonia type-specific codec id.
fn symphonia_codec_diagnostic_id(codec_params: &CodecParameters) -> String {
    match codec_params {
        CodecParameters::Audio(audio_params) => format!("audio_codec_{}", audio_params.codec),
        CodecParameters::Video(video_params) => format!("video_codec_{}", video_params.codec),
        CodecParameters::Subtitle(subtitle_params) => {
            format!("subtitle_codec_{}", subtitle_params.codec)
        }
        _ => "codec_unknown".to_string(),
    }
}

/// Возвращает стабильный unknown codec id для diagnostics и capability layer.
fn unknown_codec_id_for_kind(kind: TrackEntryKind) -> &'static str {
    match kind {
        TrackEntryKind::Supported(TrackKind::Video) => UNKNOWN_VIDEO_CODEC_ID,
        TrackEntryKind::Supported(TrackKind::Audio) => UNKNOWN_AUDIO_CODEC_ID,
        TrackEntryKind::Unsupported(_) => UNSUPPORTED_TRACK_CODEC_ID,
    }
}

/// Парсит OpusHead из codec private data.
///
/// OpusHead структура (RFC 7845):
/// 0-7:  "OpusHead" magic
/// 8:    version
/// 9:    channel count
/// 10-11: pre-skip
/// 12-15: input sample rate (LE u32)
/// 16-17: output gain
/// 18:   channel mapping family
///
/// Возвращает (sample_rate, channels) если OpusHead валиден.
fn parse_opus_head(codec_private_bytes: &[u8]) -> Option<(u32, u32)> {
    if codec_private_bytes.len() < 19 {
        return None;
    }

    if &codec_private_bytes[0..8] != b"OpusHead" {
        return None;
    }

    let channel_count = codec_private_bytes[9] as u32;
    if channel_count == 0 || channel_count > 255 {
        return None;
    }

    let sample_rate = u32::from_le_bytes([
        codec_private_bytes[12],
        codec_private_bytes[13],
        codec_private_bytes[14],
        codec_private_bytes[15],
    ]);
    if sample_rate == 0 {
        return None;
    }

    tracing::debug!(
        sample_rate,
        channels = channel_count,
        "OpusHead распарсен из codec private data"
    );

    Some((sample_rate, channel_count))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use media_core::{TrackId, TrackKind, VideoTrackMetadata};
    use symphonia::core::codecs::CodecParameters;
    use symphonia::core::codecs::audio::AudioCodecParameters;
    use symphonia::core::codecs::audio::well_known as audio_codec;
    use symphonia::core::codecs::subtitle::SubtitleCodecParameters;
    use symphonia::core::codecs::subtitle::well_known as subtitle_codec;
    use symphonia::core::codecs::video::VideoCodecParameters;
    use symphonia::core::formats::Track;
    use symphonia::core::units::{Duration as SymphoniaDuration, TimeBase};

    use super::{
        TrackEntryKind, UnsupportedTrackKind, build_track_entry, map_tracks,
        take_matroska_video_track_for_mapping,
    };
    use crate::matroska_metadata::MatroskaVideoTrack;

    fn null_video_track(track_id: u32) -> Track {
        let mut track = Track::new(track_id);
        track.with_time_base(TimeBase::try_new(1, 1_000).expect("valid time base"));
        track
    }

    fn audio_track_with_opus_head(track_id: u32) -> Track {
        let mut opus_head = [0_u8; 19];
        opus_head[0..8].copy_from_slice(b"OpusHead");
        opus_head[9] = 2;
        opus_head[12..16].copy_from_slice(&48_000_u32.to_le_bytes());

        let mut audio_params = AudioCodecParameters::new();
        audio_params.with_extra_data(opus_head.to_vec().into_boxed_slice());

        let mut track = Track::new(track_id);
        track.with_codec_params(CodecParameters::Audio(audio_params));
        track
    }

    fn audio_track_with_codec(
        track_id: u32,
        codec: symphonia::core::codecs::audio::AudioCodecId,
    ) -> Track {
        let mut audio_params = AudioCodecParameters::new();
        audio_params.for_codec(codec);

        let mut track = Track::new(track_id);
        track.with_codec_params(CodecParameters::Audio(audio_params));
        track
    }

    fn subtitle_track(track_id: u32) -> Track {
        let mut subtitle_params = SubtitleCodecParameters::new();
        subtitle_params.for_codec(subtitle_codec::CODEC_ID_WEBVTT);

        let mut track = Track::new(track_id);
        track.with_codec_params(CodecParameters::Subtitle(subtitle_params));
        track
    }

    fn vp9_video_track(track_id: u32) -> Track {
        let mut video_params = VideoCodecParameters::default();
        video_params.for_codec(symphonia::core::codecs::video::well_known::CODEC_ID_VP9);

        let mut track = Track::new(track_id);
        track.with_codec_params(CodecParameters::Video(video_params));
        track.with_time_base(TimeBase::try_new(1, 1_000).expect("valid time base"));
        track
    }

    fn vp9_video_track_with_timing(
        track_id: u32,
        duration: Option<SymphoniaDuration>,
        num_frames: Option<u64>,
    ) -> Track {
        let mut track = vp9_video_track(track_id);
        if let Some(duration) = duration {
            track.with_duration(duration);
        }
        if let Some(num_frames) = num_frames {
            track.with_num_frames(num_frames);
        }
        track
    }

    fn video_track_metadata(width: u32, height: Option<u32>) -> VideoTrackMetadata {
        VideoTrackMetadata {
            coded_width: Some(width),
            coded_height: height,
            profile: None,
            bit_depth: None,
            chroma: None,
            color: None,
        }
    }

    fn matroska_video_track(metadata: VideoTrackMetadata) -> MatroskaVideoTrack {
        MatroskaVideoTrack {
            codec_id: Some("V_VP9".to_string()),
            metadata: Some(metadata),
        }
    }

    #[test]
    fn unknown_track_without_video_metadata_is_not_assumed_to_be_video() {
        let track = null_video_track(1);

        let entry = build_track_entry(&track, None);

        assert_eq!(
            entry.kind,
            TrackEntryKind::Unsupported(UnsupportedTrackKind::Unknown)
        );
        assert_eq!(entry.codec_id, "unsupported_track");
    }

    #[test]
    fn audio_only_track_does_not_become_video() {
        let mut metadata_by_track = HashMap::new();
        let mapping = map_tracks(
            &[audio_track_with_codec(2, audio_codec::CODEC_ID_AAC)],
            &mut metadata_by_track,
        );

        assert_eq!(mapping.tracks.len(), 1);
        assert_eq!(mapping.tracks[0].kind, TrackKind::Audio);
        assert_eq!(mapping.tracks[0].codec_id, "A_AAC");
        assert!(mapping.tracks[0].video.is_none());
        assert_eq!(
            mapping
                .track_map
                .get(&2)
                .and_then(|entry| entry.supported_kind()),
            Some(TrackKind::Audio)
        );
    }

    #[test]
    fn subtitle_track_is_skipped_in_public_tracks() {
        let mut metadata_by_track = HashMap::new();
        let mapping = map_tracks(&[subtitle_track(9)], &mut metadata_by_track);

        assert!(mapping.tracks.is_empty());
        assert_eq!(
            mapping.track_map.get(&9).map(|entry| entry.kind),
            Some(TrackEntryKind::Unsupported(UnsupportedTrackKind::Subtitle))
        );
    }

    #[test]
    fn explicit_matroska_video_codec_id_wins_over_symphonia_null_codec() {
        let track = null_video_track(1);
        let matroska_video_track = MatroskaVideoTrack {
            codec_id: Some("v_vp9".to_string()),
            metadata: None,
        };

        let entry = build_track_entry(&track, Some(&matroska_video_track));

        assert_eq!(entry.supported_kind(), Some(TrackKind::Video));
        assert_eq!(entry.codec_id, "V_VP9");
    }

    #[test]
    fn symphonia_video_codec_maps_to_container_codec_id() {
        let track = vp9_video_track(1);

        let entry = build_track_entry(&track, None);

        assert_eq!(entry.supported_kind(), Some(TrackKind::Video));
        assert_eq!(entry.codec_id, "V_VP9");
    }

    #[test]
    fn opus_head_fills_missing_audio_properties() {
        let track = audio_track_with_opus_head(2);

        let entry = build_track_entry(&track, None);

        assert_eq!(entry.supported_kind(), Some(TrackKind::Audio));
        assert_eq!(entry.sample_rate, Some(48_000));
        assert_eq!(entry.channels, Some(2));
    }

    #[test]
    fn video_metadata_exact_track_id_match_is_used_first() {
        let mut metadata_by_track = HashMap::from([(
            TrackId::new(7),
            matroska_video_track(video_track_metadata(3840, None)),
        )]);

        let video_track = take_matroska_video_track_for_mapping(
            TrackId::new(7),
            TrackEntryKind::Supported(TrackKind::Video),
            2,
            &mut metadata_by_track,
        )
        .expect("exact video track metadata должна быть найдена");
        let metadata = video_track.metadata.expect("video metadata должна быть");

        assert_eq!(metadata.coded_width, Some(3840));
        assert!(metadata_by_track.is_empty());
    }

    #[test]
    fn single_matroska_video_metadata_entry_can_fallback_to_symphonia_track_id() {
        let mut metadata_by_track = HashMap::from([(
            TrackId::new(1),
            matroska_video_track(video_track_metadata(3840, Some(2160))),
        )]);

        let video_track = take_matroska_video_track_for_mapping(
            TrackId::new(0),
            TrackEntryKind::Supported(TrackKind::Video),
            2,
            &mut metadata_by_track,
        )
        .expect("single video track metadata fallback должен сработать");
        let metadata = video_track.metadata.expect("video metadata должна быть");

        assert_eq!(metadata.coded_height, Some(2160));
        assert!(metadata_by_track.is_empty());
    }

    #[test]
    fn multiple_unmatched_video_metadata_entries_do_not_fallback() {
        let mut metadata_by_track = HashMap::from([
            (
                TrackId::new(1),
                matroska_video_track(VideoTrackMetadata::empty()),
            ),
            (
                TrackId::new(2),
                matroska_video_track(VideoTrackMetadata::empty()),
            ),
        ]);

        let metadata = take_matroska_video_track_for_mapping(
            TrackId::new(0),
            TrackEntryKind::Supported(TrackKind::Video),
            2,
            &mut metadata_by_track,
        );

        assert!(metadata.is_none());
        assert_eq!(metadata_by_track.len(), 2);
    }

    #[test]
    fn map_tracks_preserves_matroska_video_metadata() {
        let mut metadata_by_track = HashMap::from([(
            TrackId::new(1),
            matroska_video_track(video_track_metadata(1920, Some(1080))),
        )]);

        let mapping = map_tracks(&[vp9_video_track(1)], &mut metadata_by_track);

        assert_eq!(mapping.tracks.len(), 1);
        assert_eq!(mapping.tracks[0].codec_id, "V_VP9");
        assert_eq!(
            mapping.tracks[0]
                .video
                .as_ref()
                .and_then(|metadata| metadata.coded_height),
            Some(1080)
        );
        assert!(mapping.track_map.contains_key(&1));
    }

    #[test]
    fn single_unknown_track_can_use_matroska_video_fallback() {
        let mut metadata_by_track = HashMap::from([(
            TrackId::new(1),
            matroska_video_track(video_track_metadata(3840, Some(2160))),
        )]);

        let mapping = map_tracks(&[null_video_track(0)], &mut metadata_by_track);

        assert_eq!(mapping.tracks.len(), 1);
        assert_eq!(mapping.tracks[0].kind, TrackKind::Video);
        assert_eq!(
            mapping.tracks[0]
                .video
                .as_ref()
                .and_then(|metadata| metadata.coded_width),
            Some(3840)
        );
    }

    #[test]
    fn single_unknown_video_candidate_can_fallback_when_audio_track_exists() {
        let mut metadata_by_track = HashMap::from([(
            TrackId::new(1),
            matroska_video_track(video_track_metadata(1920, Some(1080))),
        )]);
        let audio_track = audio_track_with_codec(2, audio_codec::CODEC_ID_AAC);
        let unknown_video_track = null_video_track(0);

        let mapping = map_tracks(&[audio_track, unknown_video_track], &mut metadata_by_track);

        assert_eq!(mapping.tracks.len(), 2);
        assert_eq!(mapping.tracks[0].kind, TrackKind::Audio);
        assert_eq!(mapping.tracks[1].kind, TrackKind::Video);
        assert_eq!(
            mapping.tracks[1]
                .video
                .as_ref()
                .and_then(|metadata| metadata.coded_height),
            Some(1080)
        );
    }

    #[test]
    fn duration_uses_track_duration_not_num_frames() {
        let mut metadata_by_track = HashMap::new();
        let num_frames_only_track = vp9_video_track_with_timing(1, None, Some(48_000));

        let mapping = map_tracks(&[num_frames_only_track], &mut metadata_by_track);

        assert_eq!(mapping.tracks[0].duration, None);
        assert_eq!(mapping.duration, None);

        let timed_track =
            vp9_video_track_with_timing(1, Some(SymphoniaDuration::new(2_500)), Some(99));
        let mut empty_metadata_by_track = HashMap::new();
        let mapping = map_tracks(&[timed_track], &mut empty_metadata_by_track);

        assert_eq!(
            mapping.tracks[0].duration,
            Some(std::time::Duration::from_millis(2_500))
        );
        assert_eq!(
            mapping.duration,
            Some(std::time::Duration::from_millis(2_500))
        );
    }

    #[test]
    fn unsupported_matroska_video_codec_stays_visible_to_capability_layer() {
        let track = null_video_track(1);
        let matroska_video_track = MatroskaVideoTrack {
            codec_id: Some("V_AV1".to_string()),
            metadata: None,
        };

        let entry = build_track_entry(&track, Some(&matroska_video_track));

        assert_eq!(entry.supported_kind(), Some(TrackKind::Video));
        assert_eq!(entry.codec_id, "V_AV1");
    }
}
