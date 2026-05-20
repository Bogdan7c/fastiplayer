use std::collections::HashMap;
use std::time::Duration;

use bytes::Bytes;
use media_core::{TrackId, TrackInfo, TrackKind};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_OPUS, CODEC_ID_VORBIS};
use symphonia::core::codecs::audio::{AudioCodecId, CODEC_ID_NULL_AUDIO};
use symphonia::core::codecs::subtitle::SubtitleCodecId;
use symphonia::core::codecs::video::well_known::{CODEC_ID_AV1, CODEC_ID_VP8, CODEC_ID_VP9};
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

/// Внутренняя запись track-а, нужная packet/seek mapper-ам после открытия demuxer-а.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrackEntry {
    pub(crate) kind: TrackKind,
    pub(crate) codec_id: String,
    pub(crate) time_base: Option<SymphoniaTimeBase>,
    pub(crate) sample_rate: Option<u32>,
    pub(crate) channels: Option<u32>,
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

    for track in symphonia_tracks {
        let provisional_kind = infer_track_kind(track);
        let matroska_video_track = take_matroska_video_track_for_track_id(
            TrackId::new(track.id),
            provisional_kind,
            video_tracks_by_track,
        );
        let track_entry = build_track_entry(track, matroska_video_track.as_ref());
        let track_info = build_track_info(track, &track_entry, matroska_video_track);

        track_map.insert(track.id, track_entry);
        tracks.push(track_info);
    }

    let duration = tracks.iter().filter_map(|track| track.duration).max();

    TrackMapping {
        tracks,
        duration,
        track_map,
    }
}

/// Достаёт Matroska video track metadata для Symphonia track id.
///
/// Symphonia может использовать внутренний `track.id`, который не равен Matroska
/// `TrackNumber`. Если pre-scan нашёл ровно один video entry, fallback безопасен:
/// двусмысленности между несколькими видеотреками нет, а HDR metadata не теряется.
pub(crate) fn take_matroska_video_track_for_track_id(
    symphonia_track_id: TrackId,
    track_kind: TrackKind,
    video_tracks_by_track: &mut HashMap<TrackId, MatroskaVideoTrack>,
) -> Option<MatroskaVideoTrack> {
    if track_kind != TrackKind::Video {
        return None;
    }

    if let Some(video_track) = video_tracks_by_track.remove(&symphonia_track_id) {
        return Some(video_track);
    }

    if video_tracks_by_track.len() != 1 {
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

/// Определяет тип трека по Symphonia track type и audio признакам.
pub(crate) fn infer_track_kind(track: &Track) -> TrackKind {
    if matches!(track.track_type(), Some(TrackType::Audio)) {
        return TrackKind::Audio;
    }

    let (sample_rate, channels) = audio_properties_from_codec_params(track);
    if sample_rate.is_some() || channels.is_some() {
        TrackKind::Audio
    } else {
        TrackKind::Video
    }
}

/// Определяет тип трека и codec_id из Symphonia 0.6 codec params и Matroska CodecID.
pub(crate) fn build_track_entry(
    track: &Track,
    matroska_video_track: Option<&MatroskaVideoTrack>,
) -> TrackEntry {
    let (sample_rate, channels) = audio_properties_from_codec_params(track);
    let kind = infer_track_kind(track);
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

/// Преобразует один Symphonia track в public media-core `TrackInfo`.
fn build_track_info(
    track: &Track,
    track_entry: &TrackEntry,
    matroska_video_track: Option<MatroskaVideoTrack>,
) -> TrackInfo {
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
    let video = matroska_video_track.and_then(|video_track| video_track.metadata);

    TrackInfo {
        id: TrackId::new(track.id),
        kind: track_entry.kind,
        codec_id: track_entry.codec_id.clone(),
        codec_private,
        time_base,
        duration,
        sample_rate: track_entry.sample_rate,
        channels: track_entry.channels,
        video,
    }
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
    kind: TrackKind,
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
        Some(CodecParameters::Subtitle(_)) | Some(_) | None => None,
    }
}

/// Мапит известные audio codec ids Symphonia в container ids, ожидаемые capability layer-ом.
fn codec_id_from_symphonia_audio_codec(codec: AudioCodecId) -> Option<&'static str> {
    match codec {
        CODEC_ID_OPUS => Some("A_OPUS"),
        CODEC_ID_VORBIS => Some("A_VORBIS"),
        _ => None,
    }
}

/// Мапит известные video codec ids Symphonia в container ids, ожидаемые capability layer-ом.
fn codec_id_from_symphonia_video_codec(codec: VideoCodecId) -> Option<&'static str> {
    match codec {
        CODEC_ID_VP8 => Some("V_VP8"),
        CODEC_ID_VP9 => Some("V_VP9"),
        CODEC_ID_AV1 => Some("V_AV1"),
        _ => None,
    }
}

/// Проверяет, что Symphonia явно не знает codec.
fn codec_params_has_null_codec(codec_params: Option<&CodecParameters>) -> bool {
    match codec_params {
        Some(CodecParameters::Audio(audio_params)) => audio_params.codec == CODEC_ID_NULL_AUDIO,
        Some(CodecParameters::Video(video_params)) => video_params.codec == CODEC_ID_NULL_VIDEO,
        Some(CodecParameters::Subtitle(subtitle_params)) => {
            subtitle_params.codec == SubtitleCodecId::default()
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
fn unknown_codec_id_for_kind(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Video => UNKNOWN_VIDEO_CODEC_ID,
        TrackKind::Audio => UNKNOWN_AUDIO_CODEC_ID,
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
    use symphonia::core::codecs::video::VideoCodecParameters;
    use symphonia::core::formats::Track;
    use symphonia::core::units::TimeBase;

    use super::{build_track_entry, map_tracks, take_matroska_video_track_for_track_id};
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

    fn vp9_video_track(track_id: u32) -> Track {
        let mut video_params = VideoCodecParameters::default();
        video_params.for_codec(symphonia::core::codecs::video::well_known::CODEC_ID_VP9);

        let mut track = Track::new(track_id);
        track.with_codec_params(CodecParameters::Video(video_params));
        track.with_time_base(TimeBase::try_new(1, 1_000).expect("valid time base"));
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
    fn unknown_video_codec_is_not_assumed_to_be_vp9() {
        let track = null_video_track(1);

        let entry = build_track_entry(&track, None);

        assert_eq!(entry.kind, TrackKind::Video);
        assert_eq!(entry.codec_id, "unknown_video");
    }

    #[test]
    fn explicit_matroska_video_codec_id_wins_over_symphonia_null_codec() {
        let track = null_video_track(1);
        let matroska_video_track = MatroskaVideoTrack {
            codec_id: Some("v_vp9".to_string()),
            metadata: None,
        };

        let entry = build_track_entry(&track, Some(&matroska_video_track));

        assert_eq!(entry.codec_id, "V_VP9");
    }

    #[test]
    fn symphonia_video_codec_maps_to_container_codec_id() {
        let track = vp9_video_track(1);

        let entry = build_track_entry(&track, None);

        assert_eq!(entry.kind, TrackKind::Video);
        assert_eq!(entry.codec_id, "V_VP9");
    }

    #[test]
    fn opus_head_fills_missing_audio_properties() {
        let track = audio_track_with_opus_head(2);

        let entry = build_track_entry(&track, None);

        assert_eq!(entry.kind, TrackKind::Audio);
        assert_eq!(entry.sample_rate, Some(48_000));
        assert_eq!(entry.channels, Some(2));
    }

    #[test]
    fn video_metadata_exact_track_id_match_is_used_first() {
        let mut metadata_by_track = HashMap::from([(
            TrackId::new(7),
            matroska_video_track(video_track_metadata(3840, None)),
        )]);

        let video_track = take_matroska_video_track_for_track_id(
            TrackId::new(7),
            TrackKind::Video,
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

        let video_track = take_matroska_video_track_for_track_id(
            TrackId::new(0),
            TrackKind::Video,
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

        let metadata = take_matroska_video_track_for_track_id(
            TrackId::new(0),
            TrackKind::Video,
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
    fn unsupported_matroska_video_codec_stays_visible_to_capability_layer() {
        let track = null_video_track(1);
        let matroska_video_track = MatroskaVideoTrack {
            codec_id: Some("V_AV1".to_string()),
            metadata: None,
        };

        let entry = build_track_entry(&track, Some(&matroska_video_track));

        assert_eq!(entry.codec_id, "V_AV1");
    }
}
