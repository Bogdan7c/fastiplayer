use std::collections::HashMap;

use codec_core::{
    ColorPrimaries, ColorRange, HdrMetadata, MatrixCoefficients, TransferFunction,
    VideoColorMetadata, VideoDisplayOrientation,
};
use media_core::{
    DiscNumber, MediaMetadata, MediaTagMetadata, TrackId, TrackNumber, TvEpisodeNumber,
    TvSeasonNumber, VideoPacketFraming,
};
use symphonia::core::meta::{RawValue, StandardTag};
use tracing::debug;

use crate::symphonia_api::FormatReaderBox;

pub(super) const RUSTIPLAYER_DISPLAY_ORIENTATION_CLOCKWISE_DEGREES_TAG: &str =
    "rustiplayer.display_orientation.clockwise_degrees";
pub(super) const RUSTIPLAYER_H264_PARAMETER_SETS_IN_BAND_TAG: &str =
    "rustiplayer.video.h264.parameter_sets_in_band";
pub(super) const RUSTIPLAYER_VIDEO_COLOR_FULL_RANGE_TAG: &str =
    "rustiplayer.video.color.full_range";
pub(super) const RUSTIPLAYER_VIDEO_COLOR_MATRIX_COEFFICIENTS_H273_TAG: &str =
    "rustiplayer.video.color.matrix_coefficients_h273";
pub(super) const RUSTIPLAYER_VIDEO_COLOR_PRIMARIES_H273_TAG: &str =
    "rustiplayer.video.color.primaries_h273";
pub(super) const RUSTIPLAYER_VIDEO_COLOR_TRANSFER_CHARACTERISTICS_H273_TAG: &str =
    "rustiplayer.video.color.transfer_characteristics_h273";
pub(super) const RUSTIPLAYER_VIDEO_HDR_MAX_LUMINANCE_NITS_TAG: &str =
    "rustiplayer.video.hdr.mastering_display.max_luminance_nits";
pub(super) const RUSTIPLAYER_VIDEO_HDR_MIN_LUMINANCE_NITS_TAG: &str =
    "rustiplayer.video.hdr.mastering_display.min_luminance_nits";
pub(super) const RUSTIPLAYER_VIDEO_HDR_MAX_CLL_NITS_TAG: &str =
    "rustiplayer.video.hdr.max_content_light_level_nits";
pub(super) const RUSTIPLAYER_VIDEO_HDR_MAX_FALL_NITS_TAG: &str =
    "rustiplayer.video.hdr.max_frame_average_light_level_nits";

/// Короткая сводка того, что Symphonia 0.6 уже принесла на format-level boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SymphoniaFormatMetadataSummary {
    /// Количество attachments, которые Symphonia отдаёт через `FormatReader`.
    pub(super) attachments: usize,

    /// Есть ли chapters в Symphonia `FormatReader`.
    pub(super) has_chapters: bool,

    /// Есть ли текущая metadata revision в Symphonia metadata log.
    pub(super) has_metadata_revision: bool,
}

/// Применяет все доступные format-level revisions как upsert.
pub(crate) fn consume_media_metadata(
    format: &mut FormatReaderBox<'_>,
    current: &mut MediaMetadata,
) -> bool {
    let before = current.clone();
    loop {
        if let Some(revision) = format.metadata().current() {
            let mut tags = MediaTagMetadata::default();
            for tag in &revision.media.tags {
                if let Some(standard_tag) = tag.std.as_ref() {
                    apply_standard_tag(&mut tags, standard_tag);
                }
            }
            current.tags.upsert(tags);
        }
        if format.metadata().is_latest() {
            break;
        }
        format.metadata().pop();
    }
    *current != before
}

/// Адаптирует один уже типизированный Symphonia tag в нейтральный media contract.
fn apply_standard_tag(tags: &mut MediaTagMetadata, standard_tag: &StandardTag) {
    match standard_tag {
        StandardTag::TrackTitle(value) | StandardTag::MovieTitle(value) => {
            tags.title = Some(value.to_string());
        }
        StandardTag::Artist(value) | StandardTag::AlbumArtist(value) => {
            tags.artists.push(value.to_string());
        }
        StandardTag::Album(value) => {
            tags.album = Some(value.to_string());
        }
        StandardTag::DiscNumber(value) => {
            tags.disc_number = Some(DiscNumber::new(*value));
        }
        StandardTag::TrackNumber(value) => {
            tags.track_number = Some(TrackNumber::new(*value));
        }
        StandardTag::TvSeasonNumber(value) => {
            tags.tv_season_number = Some(TvSeasonNumber::new(*value));
        }
        StandardTag::TvEpisodeNumber(value) => {
            tags.tv_episode_number = Some(TvEpisodeNumber::new(*value));
        }
        _ => {}
    }
}

/// Снимает format-level diagnostics до построения neutral track model.
pub(super) fn summarize_symphonia_format_metadata(
    format: &mut FormatReaderBox<'static>,
) -> SymphoniaFormatMetadataSummary {
    let attachments = format.attachments().len();
    let has_chapters = format.chapters().is_some();
    let has_metadata_revision = format.metadata().current().is_some();

    SymphoniaFormatMetadataSummary {
        attachments,
        has_chapters,
        has_metadata_revision,
    }
}

/// Достаёт display orientation, которую MP4 patch публикует как per-track metadata.
pub(super) fn display_orientations_from_metadata(
    format: &mut FormatReaderBox<'static>,
) -> HashMap<TrackId, VideoDisplayOrientation> {
    let mut orientations_by_track = HashMap::new();
    let metadata = format.metadata();
    let Some(revision) = metadata.current() else {
        return orientations_by_track;
    };

    for per_track_metadata in &revision.per_track {
        for tag in &per_track_metadata.metadata.tags {
            if tag.raw.key != RUSTIPLAYER_DISPLAY_ORIENTATION_CLOCKWISE_DEGREES_TAG {
                continue;
            }

            let Some(clockwise_degrees) =
                display_orientation_degrees_from_raw_value(&tag.raw.value)
            else {
                debug!(
                    track_id = per_track_metadata.track_id,
                    value = %tag.raw.value,
                    "Display orientation metadata has unsupported value type"
                );
                continue;
            };

            let Some(display_orientation) =
                VideoDisplayOrientation::from_clockwise_degrees(clockwise_degrees)
            else {
                debug!(
                    track_id = per_track_metadata.track_id,
                    clockwise_degrees,
                    "Display orientation metadata is not a supported quarter-turn"
                );
                continue;
            };

            let Ok(raw_track_id) = u32::try_from(per_track_metadata.track_id) else {
                debug!(
                    track_id = per_track_metadata.track_id,
                    "Display orientation metadata track id does not fit media-core TrackId"
                );
                continue;
            };

            orientations_by_track.insert(TrackId::new(raw_track_id), display_orientation);
        }
    }

    orientations_by_track
}

/// Достаёт exact `avc3` framing, опубликованный локальным ISO BMFF patch-ем.
pub(super) fn video_packet_framings_from_metadata(
    format: &mut FormatReaderBox<'static>,
) -> HashMap<TrackId, VideoPacketFraming> {
    let mut packet_framings_by_track = HashMap::new();
    let metadata = format.metadata();
    let Some(revision) = metadata.current() else {
        return packet_framings_by_track;
    };

    for per_track_metadata in &revision.per_track {
        let Some(track_id) = track_id_from_metadata(
            per_track_metadata.track_id,
            "H.264 parameter-set placement metadata",
        ) else {
            continue;
        };
        for tag in &per_track_metadata.metadata.tags {
            if tag.raw.key != RUSTIPLAYER_H264_PARAMETER_SETS_IN_BAND_TAG {
                continue;
            }
            match bool_from_raw_value(&tag.raw.value) {
                Some(true) => {
                    packet_framings_by_track.insert(
                        track_id,
                        VideoPacketFraming::LengthPrefixedWithInBandParameterSets,
                    );
                }
                Some(false) => {}
                None => debug!(
                    track_id = per_track_metadata.track_id,
                    value = %tag.raw.value,
                    "H.264 parameter-set placement metadata has unsupported value type"
                ),
            }
        }
    }

    packet_framings_by_track
}

/// Нормализует raw Symphonia metadata value в signed clockwise degrees.
fn display_orientation_degrees_from_raw_value(raw_value: &RawValue) -> Option<i32> {
    match raw_value {
        RawValue::SignedInt(value) => i32::try_from(*value).ok(),
        RawValue::UnsignedInt(value) => i32::try_from(*value).ok(),
        _ => None,
    }
}

/// Накопитель MP4 color/HDR tags до сборки полной `VideoColorMetadata`.
#[derive(Default)]
struct Mp4VideoColorMetadataTags {
    range: Option<ColorRange>,
    matrix: Option<MatrixCoefficients>,
    primaries: Option<ColorPrimaries>,
    transfer: Option<TransferFunction>,
    max_luminance_nits: Option<f32>,
    min_luminance_nits: Option<f32>,
    max_content_light_level_nits: Option<u32>,
    max_frame_average_light_level_nits: Option<u32>,
}

impl Mp4VideoColorMetadataTags {
    /// Возвращает typed metadata только если MP4 tags содержали хотя бы одно полезное поле.
    fn into_color_metadata(self) -> Option<VideoColorMetadata> {
        let has_color_metadata = self.range.is_some()
            || self.matrix.is_some()
            || self.primaries.is_some()
            || self.transfer.is_some()
            || self.max_luminance_nits.is_some()
            || self.min_luminance_nits.is_some()
            || self.max_content_light_level_nits.is_some()
            || self.max_frame_average_light_level_nits.is_some();

        if !has_color_metadata {
            return None;
        }

        let primaries = self.primaries.unwrap_or(ColorPrimaries::Unknown);
        let transfer = self.transfer.unwrap_or(TransferFunction::Unknown);
        let hdr_metadata = mp4_hdr_metadata_from_tags(&self, primaries, transfer);

        Some(VideoColorMetadata::container(
            self.range.unwrap_or(ColorRange::Unknown),
            self.matrix.unwrap_or(MatrixCoefficients::Unknown),
            primaries,
            transfer,
            hdr_metadata,
        ))
    }
}

/// Собирает HDR side metadata из MP4 `mdcv`/`clli` tags.
fn mp4_hdr_metadata_from_tags(
    tags: &Mp4VideoColorMetadataTags,
    primaries: ColorPrimaries,
    transfer: TransferFunction,
) -> Option<HdrMetadata> {
    let has_hdr_side_metadata = tags.max_luminance_nits.is_some()
        || tags.min_luminance_nits.is_some()
        || tags.max_content_light_level_nits.is_some()
        || tags.max_frame_average_light_level_nits.is_some();

    has_hdr_side_metadata.then_some(HdrMetadata {
        color_primaries: primaries,
        transfer_function: transfer,
        max_luminance_nits: tags.max_luminance_nits,
        min_luminance_nits: tags.min_luminance_nits,
        max_content_light_level_nits: tags.max_content_light_level_nits,
        max_frame_average_light_level_nits: tags.max_frame_average_light_level_nits,
    })
}

/// Достаёт MP4 color metadata, которую локальный MP4 patch публикует как per-track tags.
pub(super) fn video_color_metadata_from_metadata(
    format: &mut FormatReaderBox<'static>,
) -> HashMap<TrackId, VideoColorMetadata> {
    let mut color_tags_by_track = HashMap::<TrackId, Mp4VideoColorMetadataTags>::new();
    let metadata = format.metadata();
    let Some(revision) = metadata.current() else {
        return HashMap::new();
    };

    for per_track_metadata in &revision.per_track {
        let Some(track_id) =
            track_id_from_metadata(per_track_metadata.track_id, "MP4 color metadata")
        else {
            continue;
        };

        for tag in &per_track_metadata.metadata.tags {
            if apply_mp4_video_color_tag(
                color_tags_by_track.entry(track_id).or_default(),
                &tag.raw.key,
                &tag.raw.value,
            ) {
                continue;
            }
        }
    }

    color_tags_by_track
        .into_iter()
        .filter_map(|(track_id, tags)| {
            tags.into_color_metadata()
                .map(|color_metadata| (track_id, color_metadata))
        })
        .collect()
}

/// Применяет один raw tag к MP4 color accumulator-у и сообщает, был ли tag распознан.
fn apply_mp4_video_color_tag(
    color_tags: &mut Mp4VideoColorMetadataTags,
    tag_key: &str,
    raw_value: &RawValue,
) -> bool {
    match tag_key {
        RUSTIPLAYER_VIDEO_COLOR_FULL_RANGE_TAG => {
            match bool_from_raw_value(raw_value) {
                Some(full_range) => {
                    color_tags.range = Some(if full_range {
                        ColorRange::Full
                    } else {
                        ColorRange::Limited
                    });
                }
                None => log_unsupported_mp4_video_color_tag_value(tag_key, raw_value),
            }
            true
        }
        RUSTIPLAYER_VIDEO_COLOR_MATRIX_COEFFICIENTS_H273_TAG => {
            match u64_from_raw_value(raw_value) {
                Some(value) => {
                    color_tags.matrix = Some(MatrixCoefficients::from_h273_value(value));
                }
                None => log_unsupported_mp4_video_color_tag_value(tag_key, raw_value),
            }
            true
        }
        RUSTIPLAYER_VIDEO_COLOR_PRIMARIES_H273_TAG => {
            match u64_from_raw_value(raw_value) {
                Some(value) => {
                    color_tags.primaries = Some(ColorPrimaries::from_h273_value(value));
                }
                None => log_unsupported_mp4_video_color_tag_value(tag_key, raw_value),
            }
            true
        }
        RUSTIPLAYER_VIDEO_COLOR_TRANSFER_CHARACTERISTICS_H273_TAG => {
            match u64_from_raw_value(raw_value) {
                Some(value) => {
                    color_tags.transfer = Some(TransferFunction::from_h273_value(value));
                }
                None => log_unsupported_mp4_video_color_tag_value(tag_key, raw_value),
            }
            true
        }
        RUSTIPLAYER_VIDEO_HDR_MAX_LUMINANCE_NITS_TAG => {
            match f32_from_raw_value(raw_value) {
                Some(value) => color_tags.max_luminance_nits = Some(value),
                None => log_unsupported_mp4_video_color_tag_value(tag_key, raw_value),
            }
            true
        }
        RUSTIPLAYER_VIDEO_HDR_MIN_LUMINANCE_NITS_TAG => {
            match f32_from_raw_value(raw_value) {
                Some(value) => color_tags.min_luminance_nits = Some(value),
                None => log_unsupported_mp4_video_color_tag_value(tag_key, raw_value),
            }
            true
        }
        RUSTIPLAYER_VIDEO_HDR_MAX_CLL_NITS_TAG => {
            match u32_from_raw_value(raw_value) {
                Some(value) => color_tags.max_content_light_level_nits = Some(value),
                None => log_unsupported_mp4_video_color_tag_value(tag_key, raw_value),
            }
            true
        }
        RUSTIPLAYER_VIDEO_HDR_MAX_FALL_NITS_TAG => {
            match u32_from_raw_value(raw_value) {
                Some(value) => color_tags.max_frame_average_light_level_nits = Some(value),
                None => log_unsupported_mp4_video_color_tag_value(tag_key, raw_value),
            }
            true
        }
        _ => false,
    }
}

/// Логирует recognized MP4 color tag, значение которого нельзя безопасно нормализовать.
fn log_unsupported_mp4_video_color_tag_value(tag_key: &str, raw_value: &RawValue) {
    debug!(
        tag_key,
        value = %raw_value,
        "MP4 video color metadata tag has unsupported raw value"
    );
}

/// Нормализует raw Symphonia metadata track id в `media-core::TrackId`.
fn track_id_from_metadata(raw_track_id: u64, metadata_kind: &str) -> Option<TrackId> {
    let Ok(track_id) = u32::try_from(raw_track_id) else {
        debug!(
            track_id = raw_track_id,
            metadata_kind, "Per-track metadata id does not fit media-core TrackId"
        );
        return None;
    };

    Some(TrackId::new(track_id))
}

/// Читает boolean metadata value без угадывания произвольных строк.
fn bool_from_raw_value(raw_value: &RawValue) -> Option<bool> {
    match raw_value {
        RawValue::Boolean(value) => Some(*value),
        RawValue::SignedInt(value) => match value {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        },
        RawValue::UnsignedInt(value) => match value {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        },
        _ => None,
    }
}

/// Читает unsigned metadata value из integer raw tags.
fn u64_from_raw_value(raw_value: &RawValue) -> Option<u64> {
    match raw_value {
        RawValue::SignedInt(value) => u64::try_from(*value).ok(),
        RawValue::UnsignedInt(value) => Some(*value),
        _ => None,
    }
}

/// Читает `u32` metadata value с overflow protection.
fn u32_from_raw_value(raw_value: &RawValue) -> Option<u32> {
    u64_from_raw_value(raw_value).and_then(|value| u32::try_from(value).ok())
}

/// Читает floating-point metadata value, принимая integer tags как точные nits.
fn f32_from_raw_value(raw_value: &RawValue) -> Option<f32> {
    match raw_value {
        RawValue::Float(value) if value.is_finite() && *value >= 0.0 => Some(*value as f32),
        RawValue::SignedInt(value) => u64::try_from(*value).ok().map(|value| value as f32),
        RawValue::UnsignedInt(value) => Some(*value as f32),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn typed_sequence_standard_tags_map_to_distinct_neutral_types() {
        let mut tags = MediaTagMetadata::default();

        apply_standard_tag(&mut tags, &StandardTag::DiscNumber(2));
        apply_standard_tag(&mut tags, &StandardTag::TrackNumber(8));
        apply_standard_tag(&mut tags, &StandardTag::TvSeasonNumber(3));
        apply_standard_tag(&mut tags, &StandardTag::TvEpisodeNumber(11));

        assert_eq!(tags.disc_number, Some(DiscNumber::new(2)));
        assert_eq!(tags.track_number, Some(TrackNumber::new(8)));
        assert_eq!(tags.tv_season_number, Some(TvSeasonNumber::new(3)));
        assert_eq!(tags.tv_episode_number, Some(TvEpisodeNumber::new(11)));
    }

    #[test]
    fn existing_textual_standard_tag_mappings_remain_unchanged() {
        let mut tags = MediaTagMetadata::default();

        apply_standard_tag(
            &mut tags,
            &StandardTag::TrackTitle(Arc::new("Track title".into())),
        );
        apply_standard_tag(
            &mut tags,
            &StandardTag::MovieTitle(Arc::new("Movie title".into())),
        );
        apply_standard_tag(&mut tags, &StandardTag::Artist(Arc::new("Artist".into())));
        apply_standard_tag(
            &mut tags,
            &StandardTag::AlbumArtist(Arc::new("Album artist".into())),
        );
        apply_standard_tag(&mut tags, &StandardTag::Album(Arc::new("Album".into())));

        assert_eq!(tags.title.as_deref(), Some("Movie title"));
        assert_eq!(tags.artists, ["Artist", "Album artist"]);
        assert_eq!(tags.album.as_deref(), Some("Album"));
    }

    #[test]
    fn unrelated_standard_tag_is_a_no_op() {
        let mut tags = MediaTagMetadata {
            track_number: Some(TrackNumber::new(4)),
            ..Default::default()
        };
        let before = tags.clone();

        apply_standard_tag(&mut tags, &StandardTag::Bpm(120));

        assert_eq!(tags, before);
    }
}
