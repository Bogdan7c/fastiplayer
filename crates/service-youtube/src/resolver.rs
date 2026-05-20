use std::time::Duration;

use anyhow::{Context, Result};
use codec_core::{
    Av1Profile, BitDepth, ChromaSubsampling, ColorMetadataConfidence, ColorMetadataOrigin,
    ColorPrimaries, ColorRange, H264Profile, MatrixCoefficients, TransferFunction, VideoCodec,
    VideoColorMetadata, VideoDecodeRequirement, VideoProfile, Vp9Profile,
};
use rustiplayer_config::YoutubeConfig;
use source_core::{HttpHeader, SourceValidators};

use crate::dto::{
    YoutubeDirectStreamDescriptor, YoutubeDirectStreams, YoutubeInsufficientVideoMetadata,
    YoutubeStreamCandidate, YoutubeStreamCandidates, YoutubeStreamKind, YoutubeVideoRequirement,
    YtDlpFormat, YtDlpMetadata,
};
use crate::process::{
    YtDlpProcessConfig, resolve_youtube_candidate_metadata, resolve_youtube_metadata,
};

/// Resolver direct stream descriptors для production и тестового refresh path.
pub(crate) trait YoutubeDirectStreamResolver: Send + Sync {
    /// Возвращает свежую пару direct stream descriptors для исходного YouTube URL.
    fn resolve_direct_streams(&self, video_url: &str) -> Result<YoutubeDirectStreams>;
}

/// Production resolver на базе `yt-dlp`.
pub(crate) struct YtDlpDirectStreamResolver {
    /// Process policy для всех metadata refresh-ов этого resolver-а.
    process_config: YtDlpProcessConfig,
}

impl YtDlpDirectStreamResolver {
    /// Создаёт production resolver из пользовательского YouTube config.
    pub(crate) fn from_youtube_config(youtube_config: &YoutubeConfig) -> Result<Self> {
        Ok(Self {
            process_config: YtDlpProcessConfig::from_youtube_config(youtube_config)?,
        })
    }
}

impl YoutubeDirectStreamResolver for YtDlpDirectStreamResolver {
    fn resolve_direct_streams(&self, video_url: &str) -> Result<YoutubeDirectStreams> {
        resolve_youtube_direct_streams_with_process_config(video_url, &self.process_config)
    }
}

/// Получает normalized descriptors через `yt-dlp`, не открывая media bytes.
pub fn resolve_youtube_direct_streams(video_url: &str) -> Result<YoutubeDirectStreams> {
    let process_config = YtDlpProcessConfig::from_youtube_config(&YoutubeConfig::default())?;

    resolve_youtube_direct_streams_with_process_config(video_url, &process_config)
}

/// Получает capability-aware stream candidates через `yt-dlp`, не открывая media bytes.
pub fn resolve_youtube_stream_candidates(video_url: &str) -> Result<YoutubeStreamCandidates> {
    resolve_youtube_stream_candidates_with_config(video_url, &YoutubeConfig::default())
}

/// Получает capability-aware stream candidates с явной process policy.
pub fn resolve_youtube_stream_candidates_with_config(
    video_url: &str,
    youtube_config: &YoutubeConfig,
) -> Result<YoutubeStreamCandidates> {
    let process_config = YtDlpProcessConfig::from_youtube_config(youtube_config)?;

    resolve_youtube_stream_candidates_with_process_config(video_url, &process_config)
}

/// Получает normalized descriptors через `yt-dlp` с уже валидированной process policy.
fn resolve_youtube_direct_streams_with_process_config(
    video_url: &str,
    process_config: &YtDlpProcessConfig,
) -> Result<YoutubeDirectStreams> {
    let metadata = resolve_youtube_metadata(video_url, process_config)?;

    select_direct_media_streams(&metadata)
}

/// Получает full manifest metadata и строит service candidates.
fn resolve_youtube_stream_candidates_with_process_config(
    video_url: &str,
    process_config: &YtDlpProcessConfig,
) -> Result<YoutubeStreamCandidates> {
    let metadata = resolve_youtube_candidate_metadata(video_url, process_config)?;

    build_stream_candidates(&metadata)
}

/// Нормализует yt-dlp formats в stream candidates без выбора video default-а.
pub(crate) fn build_stream_candidates(metadata: &YtDlpMetadata) -> Result<YoutubeStreamCandidates> {
    let manifest_formats = metadata
        .formats
        .as_ref()
        .filter(|formats| !formats.is_empty())
        .or_else(|| selected_requested_formats(metadata))
        .context("yt-dlp metadata не содержит formats для stream candidates")?;
    let service_media_id = metadata.id.clone();
    let media_duration = duration_from_seconds(metadata.duration);
    let live = metadata_is_live(metadata);
    let audio_format = select_companion_audio_format(manifest_formats);
    let mut candidates = Vec::new();

    for (video_index, video_format) in manifest_formats
        .iter()
        .filter(|format| is_video_only_format(format))
        .enumerate()
    {
        let audio = audio_format.clone().map(|format| {
            direct_stream_from_format(
                YoutubeStreamKind::Audio,
                format,
                service_media_id.clone(),
                media_duration,
                live,
            )
        });
        let stream_id = candidate_stream_id(
            service_media_id.as_deref(),
            video_format.format_id.as_deref(),
            audio_format
                .as_ref()
                .and_then(|format| format.format_id.as_deref()),
            video_index,
        );
        let format_id = composite_candidate_format_id(
            video_format.format_id.as_deref(),
            audio_format
                .as_ref()
                .and_then(|format| format.format_id.as_deref()),
        );
        let video = direct_stream_from_format(
            YoutubeStreamKind::Video,
            video_format.clone(),
            service_media_id.clone(),
            media_duration,
            live,
        );

        candidates.push(YoutubeStreamCandidate {
            stream_id,
            format_id,
            video,
            audio,
            video_requirement: video_requirement_from_format(video_format),
            quality_score: video_quality_score(video_format),
        });
    }

    Ok(YoutubeStreamCandidates {
        title: metadata.title.clone(),
        service_media_id,
        duration: media_duration,
        live,
        candidates,
    })
}

/// Выбирает прямые video/audio descriptors из metadata.
pub(crate) fn select_direct_media_streams(
    metadata: &YtDlpMetadata,
) -> Result<YoutubeDirectStreams> {
    let requested_formats = metadata
        .requested_downloads
        .as_ref()
        .and_then(|downloads| downloads.first())
        .and_then(|download| download.requested_formats.as_ref())
        .or(metadata.requested_formats.as_ref())
        .context("yt-dlp metadata не содержит requested_formats для streaming")?;

    // Video stream имеет настоящий vcodec и не имеет audio codec.
    let video_format = requested_formats
        .iter()
        .find(|format| {
            format
                .vcodec
                .as_deref()
                .is_some_and(|codec| codec != "none")
                && format.acodec.as_deref().unwrap_or("none") == "none"
        })
        .cloned()
        .context("yt-dlp не вернул video-only stream URL")?;

    // Audio stream имеет настоящий acodec и не имеет video codec.
    let audio_format = requested_formats
        .iter()
        .find(|format| {
            format
                .acodec
                .as_deref()
                .is_some_and(|codec| codec != "none")
                && format.vcodec.as_deref().unwrap_or("none") == "none"
        })
        .cloned()
        .context("yt-dlp не вернул audio-only stream URL")?;

    let service_media_id = metadata.id.clone();
    let duration = duration_from_seconds(metadata.duration);
    let live = metadata_is_live(metadata);

    Ok(YoutubeDirectStreams {
        title: metadata.title.clone(),
        service_media_id: service_media_id.clone(),
        format_id: metadata.format_id.clone(),
        height: metadata.height,
        fps: metadata.fps,
        vcodec: metadata.vcodec.clone(),
        acodec: metadata.acodec.clone(),
        duration,
        live,
        video: direct_stream_from_format(
            YoutubeStreamKind::Video,
            video_format,
            service_media_id.clone(),
            duration,
            live,
        ),
        audio: direct_stream_from_format(
            YoutubeStreamKind::Audio,
            audio_format,
            service_media_id,
            duration,
            live,
        ),
    })
}

/// Возвращает выбранные yt-dlp formats старого path как fallback для tests/compat metadata.
fn selected_requested_formats(metadata: &YtDlpMetadata) -> Option<&Vec<YtDlpFormat>> {
    metadata
        .requested_downloads
        .as_ref()
        .and_then(|downloads| downloads.first())
        .and_then(|download| download.requested_formats.as_ref())
        .or(metadata.requested_formats.as_ref())
}

/// Проверяет, что format является direct video-only stream-ом.
fn is_video_only_format(format: &YtDlpFormat) -> bool {
    has_direct_media_url(format)
        && format
            .vcodec
            .as_deref()
            .is_some_and(|codec| !codec.eq_ignore_ascii_case("none"))
        && format
            .acodec
            .as_deref()
            .is_none_or(|codec| codec.eq_ignore_ascii_case("none"))
}

/// Проверяет, что format является direct audio-only stream-ом.
fn is_audio_only_format(format: &YtDlpFormat) -> bool {
    has_direct_media_url(format)
        && format
            .acodec
            .as_deref()
            .is_some_and(|codec| !codec.eq_ignore_ascii_case("none"))
        && format
            .vcodec
            .as_deref()
            .is_none_or(|codec| codec.eq_ignore_ascii_case("none"))
}

/// Отсекает manifest entries, которые нельзя открыть как direct media URL.
fn has_direct_media_url(format: &YtDlpFormat) -> bool {
    !format.url.trim().is_empty()
}

/// Выбирает companion audio без влияния на video capability selection.
fn select_companion_audio_format(formats: &[YtDlpFormat]) -> Option<YtDlpFormat> {
    formats
        .iter()
        .filter(|format| is_audio_only_format(format))
        .max_by(|left, right| {
            audio_quality_score(left)
                .cmp(&audio_quality_score(right))
                .then_with(|| left.format_id.cmp(&right.format_id))
        })
        .cloned()
}

/// Формирует stable id candidate-а из service media id и format ids.
fn candidate_stream_id(
    service_media_id: Option<&str>,
    video_format_id: Option<&str>,
    audio_format_id: Option<&str>,
    video_index: usize,
) -> String {
    let media_id = service_media_id.unwrap_or("unknown-media");
    let video_id = video_format_id
        .map(|format_id| format!("v{format_id}"))
        .unwrap_or_else(|| format!("v-index-{video_index}"));
    let audio_id = audio_format_id
        .map(|format_id| format!("a{format_id}"))
        .unwrap_or_else(|| "a-none".to_string());

    format!("{media_id}:{video_id}:{audio_id}")
}

/// Формирует composite format id пары video/audio.
fn composite_candidate_format_id(
    video_format_id: Option<&str>,
    audio_format_id: Option<&str>,
) -> Option<String> {
    match (video_format_id, audio_format_id) {
        (Some(video_format_id), Some(audio_format_id)) => {
            Some(format!("{video_format_id}+{audio_format_id}"))
        }
        (Some(video_format_id), None) => Some(video_format_id.to_string()),
        (None, Some(audio_format_id)) => Some(audio_format_id.to_string()),
        (None, None) => None,
    }
}

/// Считает deterministic quality score без проверки hardware support.
fn video_quality_score(format: &YtDlpFormat) -> i64 {
    let height_score = i64::from(format.height.unwrap_or(0)) * 1_000_000;
    let width_score = i64::from(format.width.unwrap_or(0)) * 1_000;
    let fps_score = finite_non_negative(format.fps)
        .map(|fps| (fps * 1_000.0).round() as i64)
        .unwrap_or(0);
    let bitrate_score = finite_non_negative(format.vbr.or(format.tbr))
        .map(|bitrate| bitrate.round() as i64)
        .unwrap_or(0);

    height_score + width_score + fps_score + bitrate_score
}

/// Считает deterministic audio score для companion stream-а.
fn audio_quality_score(format: &YtDlpFormat) -> i64 {
    finite_non_negative(format.abr.or(format.tbr))
        .map(|bitrate| (bitrate * 1_000.0).round() as i64)
        .unwrap_or(0)
}

/// Принимает только конечные неотрицательные float metadata значения.
fn finite_non_negative(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value >= 0.0)
}

/// Конвертирует yt-dlp video metadata в requirement или typed insufficiency.
fn video_requirement_from_format(format: &YtDlpFormat) -> YoutubeVideoRequirement {
    let Some(codec_tag) = normalized_video_codec_tag(format) else {
        return insufficient_requirement("yt-dlp не сообщил video codec", None);
    };

    let parsed_requirement = if codec_tag == "vp9"
        || codec_tag.starts_with("vp9.")
        || codec_tag.starts_with("vp09.")
    {
        vp9_requirement_from_codec_tag(&codec_tag, format)
    } else if codec_tag.starts_with("av01.") {
        av1_requirement_from_codec_tag(&codec_tag, format)
    } else if codec_tag == "h264"
        || codec_tag == "h.264"
        || codec_tag.starts_with("avc1")
        || codec_tag.starts_with("avc3")
    {
        h264_requirement_from_codec_tag(&codec_tag, format)
    } else {
        let known_codec = VideoCodec::from_container_codec_id(&codec_tag);
        let partial_requirement = known_codec.map(VideoDecodeRequirement::new);

        insufficient_requirement(
            format!(
                "yt-dlp сообщил codec `{codec_tag}`, который service-youtube пока не конвертирует"
            ),
            partial_requirement,
        )
    };

    reject_hdr_hint_without_resolved_hdr_metadata(parsed_requirement, format)
}

/// Нормализует raw vcodec, отбрасывая `none` и пустые значения.
fn normalized_video_codec_tag(format: &YtDlpFormat) -> Option<String> {
    let codec_tag = format.vcodec.as_deref()?.trim();

    (!codec_tag.is_empty() && !codec_tag.eq_ignore_ascii_case("none"))
        .then(|| codec_tag.to_ascii_lowercase())
}

/// Не даёт HDR manifest hint пройти как SDR-ready requirement без color metadata.
fn reject_hdr_hint_without_resolved_hdr_metadata(
    requirement: YoutubeVideoRequirement,
    format: &YtDlpFormat,
) -> YoutubeVideoRequirement {
    if !format_has_hdr_dynamic_range_hint(format) {
        return requirement;
    }

    match requirement {
        YoutubeVideoRequirement::Ready(requirement) if !requirement.hdr => {
            insufficient_requirement(
                "yt-dlp сообщил HDR dynamic_range, но codec tag не содержит HDR color metadata",
                Some(requirement),
            )
        }
        other_requirement => other_requirement,
    }
}

/// Проверяет текстовый yt-dlp dynamic_range hint без принятия решения о HDR path.
fn format_has_hdr_dynamic_range_hint(format: &YtDlpFormat) -> bool {
    format
        .dynamic_range
        .as_deref()
        .is_some_and(|dynamic_range| {
            let normalized_dynamic_range = dynamic_range.to_ascii_lowercase();

            normalized_dynamic_range.contains("hdr")
                || normalized_dynamic_range.contains("hlg")
                || normalized_dynamic_range.contains("pq")
        })
}

/// Строит VP9 requirement из простого `vp9` или ISO BMFF `vp09.*` tag.
fn vp9_requirement_from_codec_tag(
    codec_tag: &str,
    format: &YtDlpFormat,
) -> YoutubeVideoRequirement {
    let mut requirement = VideoDecodeRequirement::new(VideoCodec::Vp9);
    let mut missing_fields = Vec::new();

    if codec_tag == "vp9" {
        missing_fields.extend(["profile", "bit_depth", "chroma"]);
        return ready_or_insufficient(
            apply_common_video_fields(requirement, format),
            missing_fields,
        );
    }

    let codec_parts = codec_tag.split('.').collect::<Vec<_>>();
    let profile = parse_vp9_profile(&codec_parts)
        .or_else(|| parse_legacy_vp9_profile(codec_tag))
        .map(VideoProfile::Vp9);

    match profile {
        Some(profile) => {
            requirement = requirement.with_profile(profile);
        }
        None => missing_fields.push("profile"),
    }

    match codec_parts.get(3).and_then(|value| parse_decimal_u8(value)) {
        Some(bits) => match BitDepth::from_bits(bits) {
            Some(bit_depth) => {
                requirement = requirement.with_bit_depth(bit_depth);
            }
            None => {
                return insufficient_requirement(
                    format!("VP9 codec tag `{codec_tag}` содержит неподдержанную bit depth {bits}"),
                    Some(apply_common_video_fields(requirement, format)),
                );
            }
        },
        None => missing_fields.push("bit_depth"),
    }

    match parse_vp9_chroma(&codec_parts, requirement.profile) {
        Some(chroma) => {
            requirement = requirement.with_chroma(chroma);
        }
        None => missing_fields.push("chroma"),
    }

    match parse_vp9_color_metadata(&codec_parts) {
        Ok(Some(color_metadata)) => {
            requirement = requirement.with_color(color_metadata);
        }
        Ok(None) => {}
        Err(reason) => {
            return insufficient_requirement(
                reason,
                Some(apply_common_video_fields(requirement, format)),
            );
        }
    }

    ready_or_insufficient(
        apply_common_video_fields(requirement, format),
        missing_fields,
    )
}

/// Строит AV1 requirement из `av01.*` codec tag.
fn av1_requirement_from_codec_tag(
    codec_tag: &str,
    format: &YtDlpFormat,
) -> YoutubeVideoRequirement {
    let mut requirement = VideoDecodeRequirement::new(VideoCodec::Av1);
    let mut missing_fields = Vec::new();
    let codec_parts = codec_tag.split('.').collect::<Vec<_>>();

    match codec_parts.get(1).and_then(|value| parse_decimal_u8(value)) {
        Some(0) => {
            requirement = requirement.with_profile(VideoProfile::Av1(Av1Profile::Main));
        }
        Some(1) => {
            requirement = requirement.with_profile(VideoProfile::Av1(Av1Profile::High));
        }
        Some(2) => {
            requirement = requirement.with_profile(VideoProfile::Av1(Av1Profile::Professional));
        }
        Some(profile) => {
            return insufficient_requirement(
                format!("AV1 codec tag `{codec_tag}` содержит неподдержанный profile {profile}"),
                Some(apply_common_video_fields(requirement, format)),
            );
        }
        None => missing_fields.push("profile"),
    }

    match codec_parts.get(3).and_then(|value| parse_decimal_u8(value)) {
        Some(bits) => match BitDepth::from_bits(bits) {
            Some(bit_depth) => {
                requirement = requirement.with_bit_depth(bit_depth);
            }
            None => {
                return insufficient_requirement(
                    format!("AV1 codec tag `{codec_tag}` содержит неподдержанную bit depth {bits}"),
                    Some(apply_common_video_fields(requirement, format)),
                );
            }
        },
        None => missing_fields.push("bit_depth"),
    }

    match parse_av1_chroma(&codec_parts) {
        Ok(Some(chroma)) => {
            requirement = requirement.with_chroma(chroma);
        }
        Ok(None) => missing_fields.push("chroma"),
        Err(reason) => {
            return insufficient_requirement(
                reason,
                Some(apply_common_video_fields(requirement, format)),
            );
        }
    }

    match parse_av1_color_metadata(&codec_parts) {
        Ok(color_metadata) => {
            requirement = requirement.with_color(color_metadata);
        }
        Err(reason) => {
            return insufficient_requirement(
                reason,
                Some(apply_common_video_fields(requirement, format)),
            );
        }
    }

    ready_or_insufficient(
        apply_common_video_fields(requirement, format),
        missing_fields,
    )
}

/// Строит H.264 requirement из AVC `avc1.PPCCLL`/`avc3.PPCCLL` codec tag.
fn h264_requirement_from_codec_tag(
    codec_tag: &str,
    format: &YtDlpFormat,
) -> YoutubeVideoRequirement {
    let mut requirement = VideoDecodeRequirement::new(VideoCodec::H264);

    if codec_tag == "h264" || codec_tag == "h.264" {
        return insufficient_requirement(
            "H.264 metadata не содержит AVC profile_idc".to_string(),
            Some(apply_common_video_fields(requirement, format)),
        );
    }

    let codec_parts = codec_tag.split('.').collect::<Vec<_>>();
    let Some(avcoti) = codec_parts.get(1).copied() else {
        return insufficient_requirement(
            format!("H.264 codec tag `{codec_tag}` не содержит avcoti"),
            Some(apply_common_video_fields(requirement, format)),
        );
    };
    let Some((profile_idc, constraint_flags)) = parse_avcoti(avcoti) else {
        return insufficient_requirement(
            format!("H.264 codec tag `{codec_tag}` содержит некорректный avcoti"),
            Some(apply_common_video_fields(requirement, format)),
        );
    };

    let profile = match profile_idc {
        0x42 if constraint_flags & 0x40 != 0 => {
            VideoProfile::H264(H264Profile::ConstrainedBaseline)
        }
        0x4D => VideoProfile::H264(H264Profile::Main),
        0x64 => VideoProfile::H264(H264Profile::High),
        _ => {
            return insufficient_requirement(
                format!("H.264 profile_idc 0x{profile_idc:02x} не представлен в codec-core model"),
                Some(apply_common_video_fields(requirement, format)),
            );
        }
    };

    requirement = requirement
        .with_profile(profile)
        .with_bit_depth(BitDepth::Eight)
        .with_chroma(ChromaSubsampling::Yuv420);

    ready_or_insufficient(apply_common_video_fields(requirement, format), Vec::new())
}

/// Добавляет resolution/fps только когда service metadata уже валидна.
fn apply_common_video_fields(
    mut requirement: VideoDecodeRequirement,
    format: &YtDlpFormat,
) -> VideoDecodeRequirement {
    if let (Some(width), Some(height)) = (format.width, format.height) {
        requirement = requirement.with_resolution(width, height);
    }

    if let Some(frame_rate) = finite_non_negative(format.fps) {
        requirement = requirement.with_frame_rate(frame_rate);
    }

    requirement
}

/// Возвращает ready requirement или insufficient с сохранением partial requirement.
fn ready_or_insufficient(
    requirement: VideoDecodeRequirement,
    missing_fields: Vec<&'static str>,
) -> YoutubeVideoRequirement {
    if missing_fields.is_empty() {
        return YoutubeVideoRequirement::Ready(requirement);
    }

    insufficient_requirement(
        format!(
            "yt-dlp video metadata неполная: отсутствует {}",
            missing_fields.join(", ")
        ),
        Some(requirement),
    )
}

/// Создаёт typed insufficiency без превращения её в generic error.
fn insufficient_requirement(
    reason: impl Into<String>,
    partial_requirement: Option<VideoDecodeRequirement>,
) -> YoutubeVideoRequirement {
    YoutubeVideoRequirement::Insufficient(YoutubeInsufficientVideoMetadata {
        reason: reason.into(),
        partial_requirement,
    })
}

/// Парсит VP9 profile из ISO BMFF codec tag parts.
fn parse_vp9_profile(codec_parts: &[&str]) -> Option<Vp9Profile> {
    codec_parts
        .get(1)
        .and_then(|value| parse_decimal_u8(value))
        .and_then(Vp9Profile::from_bitstream_value)
}

/// Парсит legacy yt-dlp hint вида `vp9.2`.
fn parse_legacy_vp9_profile(codec_tag: &str) -> Option<Vp9Profile> {
    codec_tag
        .strip_prefix("vp9.")
        .and_then(parse_decimal_u8)
        .and_then(Vp9Profile::from_bitstream_value)
}

/// Парсит VP9 chroma из explicit CC или из profile constraints.
fn parse_vp9_chroma(
    codec_parts: &[&str],
    profile: Option<VideoProfile>,
) -> Option<ChromaSubsampling> {
    if let Some(chroma_value) = codec_parts.get(4).and_then(|value| parse_decimal_u8(value)) {
        return match chroma_value {
            0 | 1 => Some(ChromaSubsampling::Yuv420),
            2 => Some(ChromaSubsampling::Yuv422),
            3 => Some(ChromaSubsampling::Yuv444),
            _ => None,
        };
    }

    match profile {
        Some(VideoProfile::Vp9(Vp9Profile::Profile0 | Vp9Profile::Profile2)) => {
            Some(ChromaSubsampling::Yuv420)
        }
        _ => None,
    }
}

/// Парсит optional VP9 color metadata, если codec tag содержит полный блок.
fn parse_vp9_color_metadata(
    codec_parts: &[&str],
) -> std::result::Result<Option<VideoColorMetadata>, String> {
    if codec_parts.len() < 9 {
        return Ok(None);
    }

    let primaries = parse_h273_primaries(codec_parts[5], "VP9 color primaries")?;
    let transfer = parse_h273_transfer(codec_parts[6], "VP9 transfer")?;
    let matrix = parse_h273_matrix(codec_parts[7], "VP9 matrix")?;
    let range = match parse_decimal_u8(codec_parts[8]) {
        Some(0) => ColorRange::Limited,
        Some(1) => ColorRange::Full,
        Some(value) => {
            return Err(format!("VP9 full range flag {value} не поддерживается"));
        }
        None => {
            return Err("VP9 full range flag отсутствует или некорректен".to_string());
        }
    };

    Ok(Some(manifest_color_metadata(
        range, matrix, primaries, transfer,
    )))
}

/// Парсит AV1 chroma или spec default для короткого `av01.P.LLT.DD`.
fn parse_av1_chroma(
    codec_parts: &[&str],
) -> std::result::Result<Option<ChromaSubsampling>, String> {
    if codec_parts.len() <= 4 {
        return Ok(Some(ChromaSubsampling::Yuv420));
    }

    let monochrome = codec_parts.get(4).copied().unwrap_or("0");
    if monochrome == "1" {
        return Err("AV1 monochrome stream не представлен в codec-core chroma model".to_string());
    }

    let Some(chroma_code) = codec_parts.get(5).copied() else {
        return Ok(None);
    };
    let chroma_digits = chroma_code.as_bytes();
    if chroma_digits.len() != 3 || !chroma_digits.iter().all(|digit| digit.is_ascii_digit()) {
        return Err(format!("AV1 chroma code `{chroma_code}` некорректен"));
    }

    match (chroma_digits[0], chroma_digits[1]) {
        (b'0', b'0') => Ok(Some(ChromaSubsampling::Yuv444)),
        (b'1', b'0') => Ok(Some(ChromaSubsampling::Yuv422)),
        (b'1', b'1') => Ok(Some(ChromaSubsampling::Yuv420)),
        _ => Err(format!("AV1 chroma code `{chroma_code}` не поддерживается")),
    }
}

/// Парсит AV1 color metadata, применяя defaults из AV1 ISOBMFF binding.
fn parse_av1_color_metadata(
    codec_parts: &[&str],
) -> std::result::Result<VideoColorMetadata, String> {
    if codec_parts.len() <= 4 {
        return Ok(manifest_color_metadata(
            ColorRange::Limited,
            MatrixCoefficients::Bt709,
            ColorPrimaries::Bt709,
            TransferFunction::Bt709,
        ));
    }

    if codec_parts.len() < 10 {
        return Err("AV1 codec tag содержит неполный optional color/chroma блок".to_string());
    }

    let primaries = parse_h273_primaries(codec_parts[6], "AV1 color primaries")?;
    let transfer = parse_h273_transfer(codec_parts[7], "AV1 transfer")?;
    let matrix = parse_h273_matrix(codec_parts[8], "AV1 matrix")?;
    let range = match codec_parts.get(9).and_then(|value| parse_decimal_u8(value)) {
        Some(0) | None => ColorRange::Limited,
        Some(1) => ColorRange::Full,
        Some(value) => {
            return Err(format!("AV1 full range flag {value} не поддерживается"));
        }
    };

    Ok(manifest_color_metadata(range, matrix, primaries, transfer))
}

/// Создаёт manifest-origin color metadata для раннего service hint-а.
fn manifest_color_metadata(
    range: ColorRange,
    matrix: MatrixCoefficients,
    primaries: ColorPrimaries,
    transfer: TransferFunction,
) -> VideoColorMetadata {
    VideoColorMetadata {
        range,
        matrix,
        primaries,
        transfer,
        hdr_metadata: None,
        origin: ColorMetadataOrigin::Manifest,
        confidence: ColorMetadataConfidence::Hint,
    }
}

/// Парсит H.273 color primaries и запрещает unknown как безопасный ранний hint.
fn parse_h273_primaries(
    value: &str,
    field_name: &str,
) -> std::result::Result<ColorPrimaries, String> {
    let Some(raw_value) = parse_decimal_u64(value) else {
        return Err(format!("{field_name} `{value}` некорректен"));
    };
    let primaries = ColorPrimaries::from_h273_value(raw_value);

    if primaries == ColorPrimaries::Unknown {
        return Err(format!("{field_name} `{value}` не поддерживается"));
    }

    Ok(primaries)
}

/// Парсит H.273 transfer и запрещает unknown как безопасный ранний hint.
fn parse_h273_transfer(
    value: &str,
    field_name: &str,
) -> std::result::Result<TransferFunction, String> {
    let Some(raw_value) = parse_decimal_u64(value) else {
        return Err(format!("{field_name} `{value}` некорректен"));
    };
    let transfer = TransferFunction::from_h273_value(raw_value);

    if transfer == TransferFunction::Unknown {
        return Err(format!("{field_name} `{value}` не поддерживается"));
    }

    Ok(transfer)
}

/// Парсит H.273 matrix и запрещает unknown как безопасный ранний hint.
fn parse_h273_matrix(
    value: &str,
    field_name: &str,
) -> std::result::Result<MatrixCoefficients, String> {
    let Some(raw_value) = parse_decimal_u64(value) else {
        return Err(format!("{field_name} `{value}` некорректен"));
    };
    let matrix = MatrixCoefficients::from_h273_value(raw_value);

    if matrix == MatrixCoefficients::Unknown {
        return Err(format!("{field_name} `{value}` не поддерживается"));
    }

    Ok(matrix)
}

/// Парсит decimal u8 component без panic.
fn parse_decimal_u8(value: &str) -> Option<u8> {
    value.parse::<u8>().ok()
}

/// Парсит decimal u64 component без panic.
fn parse_decimal_u64(value: &str) -> Option<u64> {
    value.parse::<u64>().ok()
}

/// Парсит AVC OTI `PPCCLL`, где PP - profile_idc, CC - constraint flags.
fn parse_avcoti(avcoti: &str) -> Option<(u8, u8)> {
    if avcoti.len() != 6
        || !avcoti
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }

    let profile_idc = u8::from_str_radix(&avcoti[0..2], 16).ok()?;
    let constraint_flags = u8::from_str_radix(&avcoti[2..4], 16).ok()?;

    Some((profile_idc, constraint_flags))
}

/// Преобразует JSON format в runtime stream descriptor.
fn direct_stream_from_format(
    kind: YoutubeStreamKind,
    format: YtDlpFormat,
    service_media_id: Option<String>,
    media_duration: Option<Duration>,
    live: bool,
) -> YoutubeDirectStreamDescriptor {
    let size = format
        .filesize
        .or(format.filesize_approx)
        .map(|bytes| format!("{:.1} MiB", bytes as f64 / 1024.0 / 1024.0))
        .unwrap_or_else(|| "unknown size".to_string());

    let description = format!(
        "{} format={} ext={} vcodec={} acodec={} height={} fps={} size={}",
        kind.as_str(),
        format.format_id.as_deref().unwrap_or("unknown"),
        format.ext.as_deref().unwrap_or("unknown"),
        format.vcodec.as_deref().unwrap_or("unknown"),
        format.acodec.as_deref().unwrap_or("unknown"),
        format
            .height
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        format
            .fps
            .map(|value| format!("{value:.0}"))
            .unwrap_or_else(|| "none".to_string()),
        size,
    );
    let duration = duration_from_seconds(format.duration).or(media_duration);
    let headers = format
        .http_headers
        .unwrap_or_default()
        .into_iter()
        .map(|(name, value)| HttpHeader::new(name, value))
        .collect();

    YoutubeDirectStreamDescriptor {
        kind,
        url: format.url,
        headers,
        format_id: format.format_id,
        service_media_id,
        validators: SourceValidators::default(),
        duration,
        live,
        description,
    }
}

/// Конвертирует секунды yt-dlp в `Duration`, отбрасывая некорректные значения.
fn duration_from_seconds(seconds: Option<f64>) -> Option<Duration> {
    let seconds = seconds?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }

    Some(Duration::from_secs_f64(seconds))
}

/// Определяет live media без превращения бывших live VOD в live.
fn metadata_is_live(metadata: &YtDlpMetadata) -> bool {
    metadata.is_live.unwrap_or(false)
        || matches!(
            metadata.live_status.as_deref(),
            Some("is_live" | "is_upcoming")
        )
}

/// Формирует описание выбранного streaming формата.
pub(crate) fn build_streaming_description(direct_streams: &YoutubeDirectStreams) -> String {
    let title = direct_streams.title.as_deref().unwrap_or("YouTube video");
    let video_id = direct_streams
        .service_media_id
        .as_deref()
        .unwrap_or("unknown id");
    let format_id = direct_streams
        .format_id
        .as_deref()
        .unwrap_or("unknown format");
    let height = direct_streams
        .height
        .map(|value| format!("{value}p"))
        .unwrap_or_else(|| "unknown height".to_string());
    let fps = direct_streams
        .fps
        .map(|value| format!("{value:.0}fps"))
        .unwrap_or_else(|| "unknown fps".to_string());
    let vcodec = direct_streams
        .vcodec
        .as_deref()
        .unwrap_or("unknown video codec");
    let acodec = direct_streams
        .acodec
        .as_deref()
        .unwrap_or("unknown audio codec");
    let duration = direct_streams
        .duration
        .map(|value| format!("{}s", value.as_secs()))
        .unwrap_or_else(|| "unknown duration".to_string());
    let playback_kind = if direct_streams.live { "live" } else { "vod" };

    format!(
        "{title} [{video_id}] {playback_kind} {format_id} - {height} {fps}, {vcodec} + {acodec}, {duration}; {}; {}",
        direct_streams.video.description, direct_streams.audio.description
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video_format(format_id: &str, vcodec: &str) -> YtDlpFormat {
        YtDlpFormat {
            url: format!("http://media.test/{format_id}.video"),
            format_id: Some(format_id.to_string()),
            ext: Some("webm".to_string()),
            vcodec: Some(vcodec.to_string()),
            acodec: Some("none".to_string()),
            width: Some(3840),
            height: Some(2160),
            fps: Some(60.0),
            tbr: Some(12_000.0),
            vbr: Some(12_000.0),
            abr: None,
            dynamic_range: Some("SDR".to_string()),
            filesize: None,
            filesize_approx: None,
            duration: Some(120.0),
            http_headers: None,
        }
    }

    fn audio_format(format_id: &str, abr: f64) -> YtDlpFormat {
        YtDlpFormat {
            url: format!("http://media.test/{format_id}.audio"),
            format_id: Some(format_id.to_string()),
            ext: Some("webm".to_string()),
            vcodec: Some("none".to_string()),
            acodec: Some("opus".to_string()),
            width: None,
            height: None,
            fps: None,
            tbr: Some(abr),
            vbr: None,
            abr: Some(abr),
            dynamic_range: None,
            filesize: None,
            filesize_approx: None,
            duration: Some(120.0),
            http_headers: None,
        }
    }

    fn ready_requirement(format: &YtDlpFormat) -> VideoDecodeRequirement {
        let YoutubeVideoRequirement::Ready(requirement) = video_requirement_from_format(format)
        else {
            panic!("format должен дать ready requirement");
        };

        requirement
    }

    #[test]
    fn vp9_codec_metadata_builds_video_decode_requirement() {
        let format = video_format("315", "vp09.02.10.10.01.09.16.09.00");

        let requirement = ready_requirement(&format);

        assert_eq!(requirement.codec, VideoCodec::Vp9);
        assert_eq!(
            requirement.profile,
            Some(VideoProfile::Vp9(Vp9Profile::Profile2))
        );
        assert_eq!(requirement.bit_depth, Some(BitDepth::Ten));
        assert_eq!(requirement.chroma, Some(ChromaSubsampling::Yuv420));
        assert_eq!(requirement.width, Some(3840));
        assert_eq!(requirement.height, Some(2160));
        assert_eq!(requirement.fps, Some(60.0));
        assert!(requirement.hdr);
        assert_eq!(
            requirement.color.as_ref().map(|color| color.transfer),
            Some(TransferFunction::Pq)
        );
    }

    #[test]
    fn av1_codec_metadata_builds_video_decode_requirement() {
        let mut format = video_format("399", "av01.0.08M.08");
        format.ext = Some("mp4".to_string());

        let requirement = ready_requirement(&format);

        assert_eq!(requirement.codec, VideoCodec::Av1);
        assert_eq!(
            requirement.profile,
            Some(VideoProfile::Av1(Av1Profile::Main))
        );
        assert_eq!(requirement.bit_depth, Some(BitDepth::Eight));
        assert_eq!(requirement.chroma, Some(ChromaSubsampling::Yuv420));
        assert_eq!(
            requirement.color.as_ref().map(|color| color.transfer),
            Some(TransferFunction::Bt709)
        );
        assert!(!requirement.hdr);
    }

    #[test]
    fn h264_codec_metadata_builds_video_decode_requirement() {
        let mut format = video_format("137", "avc1.640028");
        format.ext = Some("mp4".to_string());

        let requirement = ready_requirement(&format);

        assert_eq!(requirement.codec, VideoCodec::H264);
        assert_eq!(
            requirement.profile,
            Some(VideoProfile::H264(H264Profile::High))
        );
        assert_eq!(requirement.bit_depth, Some(BitDepth::Eight));
        assert_eq!(requirement.chroma, Some(ChromaSubsampling::Yuv420));
    }

    #[test]
    fn incomplete_codec_metadata_is_marked_insufficient_without_guessing() {
        let format = video_format("248", "vp9");

        let requirement = video_requirement_from_format(&format);

        let YoutubeVideoRequirement::Insufficient(metadata) = requirement else {
            panic!("plain vp9 metadata не должна становиться ready requirement");
        };
        assert!(metadata.reason.contains("profile"));
        assert!(metadata.reason.contains("bit_depth"));
        assert_eq!(
            metadata
                .partial_requirement
                .as_ref()
                .map(|requirement| requirement.codec),
            Some(VideoCodec::Vp9)
        );
        assert_eq!(
            metadata
                .partial_requirement
                .as_ref()
                .and_then(|requirement| requirement.profile),
            None
        );
    }

    #[test]
    fn stream_candidates_preserve_direct_descriptors_and_capability_candidate() {
        let metadata = YtDlpMetadata {
            title: Some("sample".to_string()),
            id: Some("abc123".to_string()),
            format_id: None,
            height: None,
            fps: None,
            vcodec: None,
            acodec: None,
            duration: Some(120.0),
            is_live: Some(false),
            live_status: None,
            requested_downloads: None,
            requested_formats: None,
            formats: Some(vec![
                video_format("315", "vp09.02.10.10.01.09.16.09.00"),
                audio_format("250", 70.0),
                audio_format("251", 160.0),
            ]),
        };

        let candidates = build_stream_candidates(&metadata).expect("candidates built");

        assert_eq!(candidates.candidates.len(), 1);
        let candidate = &candidates.candidates[0];
        assert_eq!(candidate.stream_id, "abc123:v315:a251");
        assert_eq!(candidate.format_id.as_deref(), Some("315+251"));
        assert_eq!(candidate.video.format_id.as_deref(), Some("315"));
        assert_eq!(
            candidate
                .audio
                .as_ref()
                .and_then(|audio| audio.format_id.as_deref()),
            Some("251")
        );
        assert!(candidate.video_requirement.is_ready());
        let capability_candidate = candidate
            .to_capability_candidate()
            .expect("ready metadata converts to capability candidate");
        assert_eq!(capability_candidate.stream_id, "abc123:v315:a251");
        assert_eq!(capability_candidate.quality_score, candidate.quality_score);
    }
}
