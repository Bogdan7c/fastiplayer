use std::time::Duration;

use anyhow::{Context, Result};
use codec_core::{
    Av1Profile, BitDepth, ChromaSubsampling, ColorMetadataConfidence, ColorMetadataOrigin,
    ColorPrimaries, ColorRange, H264Profile, MatrixCoefficients, TransferFunction, VideoCodec,
    VideoColorMetadata, VideoDecodeRequirement, VideoProfile, Vp9Profile,
};
use rustiplayer_config::YoutubeConfig;
use source_core::{HttpHeader, SourceValidators};

use crate::YoutubeMediaLocator;
use crate::dto::{
    YoutubeDirectStreamDescriptor, YoutubeDirectStreams, YoutubeDynamicRange,
    YoutubeInsufficientVideoMetadata, YoutubeStreamCandidate, YoutubeStreamCandidates,
    YoutubeStreamKind, YoutubeVideoRequirement, YtDlpFormat, YtDlpMetadata,
};
use crate::process::{
    YtDlpProcessConfig, resolve_youtube_candidate_metadata, resolve_youtube_metadata,
};

/// Resolver direct stream descriptors для production и тестового refresh path.
pub(crate) trait YoutubeDirectStreamResolver: Send + Sync {
    /// Возвращает свежую пару direct stream descriptors для исходного YouTube URL.
    fn resolve_direct_streams(&self, locator: &YoutubeMediaLocator)
    -> Result<YoutubeDirectStreams>;
}

/// Production resolver на базе `yt-dlp`.
pub(crate) struct YtDlpDirectStreamResolver {
    /// Process policy для всех metadata refresh-ов этого resolver-а.
    process_config: YtDlpProcessConfig,
}

/// Stable identity выбранного candidate-а для refresh-а direct URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YoutubeSelectedStreamIdentity {
    /// Stream id из первого service manifest-а.
    stream_id: String,

    /// Composite format id пары, если extractor его сообщил.
    format_id: Option<String>,

    /// Video format id, который должен сохраниться при refresh-е.
    video_format_id: Option<String>,

    /// Audio format id, который должен сохраниться при refresh-е.
    audio_format_id: Option<String>,
}

/// Resolver, который refresh-ит direct URL той же capability-selected пары.
pub(crate) struct YtDlpSelectedStreamResolver {
    /// Process policy для повторного получения manifest-а.
    process_config: YtDlpProcessConfig,

    /// Identity выбранной пары, которую нельзя заменить legacy selector-ом.
    selected_stream: YoutubeSelectedStreamIdentity,
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
    fn resolve_direct_streams(
        &self,
        locator: &YoutubeMediaLocator,
    ) -> Result<YoutubeDirectStreams> {
        resolve_youtube_direct_streams_with_process_config(locator, &self.process_config)
    }
}

impl YoutubeSelectedStreamIdentity {
    /// Запоминает выбранные ids без владения полным manifest-ом.
    #[must_use]
    pub fn from_candidate(candidate: &YoutubeStreamCandidate) -> Self {
        Self {
            stream_id: candidate.stream_id.clone(),
            format_id: candidate.format_id.clone(),
            video_format_id: candidate.video.format_id.clone(),
            audio_format_id: candidate
                .audio
                .as_ref()
                .and_then(|audio| audio.format_id.clone()),
        }
    }

    /// Находит выбранный candidate в service manifest-е и сохраняет только reconstructible ids.
    pub fn from_candidates(
        stream_candidates: &YoutubeStreamCandidates,
        selected_stream_id: &str,
    ) -> Result<Self> {
        let selected_candidate = stream_candidates
            .candidates
            .iter()
            .find(|candidate| candidate.stream_id == selected_stream_id)
            .with_context(|| {
                format!("selected YouTube stream id не найден в candidates: {selected_stream_id}")
            })?;

        Ok(Self::from_candidate(selected_candidate))
    }

    /// Stable stream id из первого manifest-а; нужен app-side diagnostics/tests.
    #[must_use]
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// Проверяет, что fresh manifest содержит ту же video/audio пару.
    fn matches_format_identity(&self, candidate: &YoutubeStreamCandidate) -> bool {
        let Some(expected_video_format_id) = self.video_format_id.as_deref() else {
            return false;
        };
        let Some(expected_audio_format_id) = self.audio_format_id.as_deref() else {
            return false;
        };

        let candidate_audio_format_id = candidate
            .audio
            .as_ref()
            .and_then(|audio| audio.format_id.as_deref());

        candidate.video.format_id.as_deref() == Some(expected_video_format_id)
            && candidate_audio_format_id == Some(expected_audio_format_id)
            && self
                .format_id
                .as_deref()
                .is_none_or(|format_id| candidate.format_id.as_deref() == Some(format_id))
    }
}

impl YtDlpSelectedStreamResolver {
    /// Создаёт refresh resolver для уже выбранной capability-aware пары.
    pub(crate) fn from_youtube_config(
        youtube_config: &YoutubeConfig,
        selected_stream: YoutubeSelectedStreamIdentity,
    ) -> Result<Self> {
        Ok(Self {
            process_config: YtDlpProcessConfig::from_youtube_config(youtube_config)?,
            selected_stream,
        })
    }
}

impl YoutubeDirectStreamResolver for YtDlpSelectedStreamResolver {
    fn resolve_direct_streams(
        &self,
        locator: &YoutubeMediaLocator,
    ) -> Result<YoutubeDirectStreams> {
        let stream_candidates =
            resolve_youtube_stream_candidates_with_process_config(locator, &self.process_config)?;

        direct_streams_from_matching_candidate(&stream_candidates, &self.selected_stream)
    }
}

/// Получает normalized descriptors через `yt-dlp`, не открывая media bytes.
pub fn resolve_youtube_direct_streams(
    locator: &YoutubeMediaLocator,
) -> Result<YoutubeDirectStreams> {
    let process_config = YtDlpProcessConfig::from_youtube_config(&YoutubeConfig::default())?;

    resolve_youtube_direct_streams_with_process_config(locator, &process_config)
}

/// Получает capability-aware stream candidates через `yt-dlp`, не открывая media bytes.
pub fn resolve_youtube_stream_candidates(
    locator: &YoutubeMediaLocator,
) -> Result<YoutubeStreamCandidates> {
    resolve_youtube_stream_candidates_with_config(locator, &YoutubeConfig::default())
}

/// Получает capability-aware stream candidates с явной process policy.
pub fn resolve_youtube_stream_candidates_with_config(
    locator: &YoutubeMediaLocator,
    youtube_config: &YoutubeConfig,
) -> Result<YoutubeStreamCandidates> {
    let process_config = YtDlpProcessConfig::from_youtube_config(youtube_config)?;

    resolve_youtube_stream_candidates_with_process_config(locator, &process_config)
}

/// Получает normalized descriptors через `yt-dlp` с уже валидированной process policy.
fn resolve_youtube_direct_streams_with_process_config(
    locator: &YoutubeMediaLocator,
    process_config: &YtDlpProcessConfig,
) -> Result<YoutubeDirectStreams> {
    let metadata = resolve_youtube_metadata(locator.expose_secret_for_open(), process_config)?;

    select_direct_media_streams(&metadata)
}

/// Получает full manifest metadata и строит service candidates.
fn resolve_youtube_stream_candidates_with_process_config(
    locator: &YoutubeMediaLocator,
    process_config: &YtDlpProcessConfig,
) -> Result<YoutubeStreamCandidates> {
    let metadata =
        resolve_youtube_candidate_metadata(locator.expose_secret_for_open(), process_config)?;

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
        .filter(|format| is_supported_video_candidate_format(format))
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
            height: video_format.height,
            fps: video_format.fps,
            vcodec: video_format.vcodec.clone(),
            acodec: audio_format
                .as_ref()
                .and_then(|format| format.acodec.clone()),
            dynamic_range: youtube_dynamic_range(video_format),
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

/// Нормализует только отдельное поле `dynamic_range`, не читая quality label/description.
fn youtube_dynamic_range(format: &YtDlpFormat) -> YoutubeDynamicRange {
    let Some(dynamic_range) = format.dynamic_range.as_deref() else {
        return YoutubeDynamicRange::Unknown;
    };

    match dynamic_range.trim().to_ascii_lowercase().as_str() {
        "sdr" => YoutubeDynamicRange::Sdr,
        "hdr" | "hdr10" | "hdr10+" | "hlg" | "pq" => YoutubeDynamicRange::Hdr,
        _ => YoutubeDynamicRange::Unknown,
    }
}

/// Берёт выбранный stream id из service candidates и строит pair descriptors.
pub(crate) fn direct_streams_from_selected_candidate(
    stream_candidates: &YoutubeStreamCandidates,
    selected_stream_id: &str,
) -> Result<YoutubeDirectStreams> {
    let selected_candidate = stream_candidates
        .candidates
        .iter()
        .find(|candidate| candidate.stream_id == selected_stream_id)
        .with_context(|| {
            format!("selected YouTube stream id не найден в candidates: {selected_stream_id}")
        })?;

    direct_streams_from_candidate(stream_candidates, selected_candidate)
}

/// Повторно находит выбранный stream после refresh-а manifest-а.
pub(crate) fn direct_streams_from_matching_candidate(
    stream_candidates: &YoutubeStreamCandidates,
    selected_stream: &YoutubeSelectedStreamIdentity,
) -> Result<YoutubeDirectStreams> {
    let selected_candidate = stream_candidates
        .candidates
        .iter()
        .find(|candidate| candidate.stream_id == selected_stream.stream_id)
        .or_else(|| {
            stream_candidates
                .candidates
                .iter()
                .find(|candidate| selected_stream.matches_format_identity(candidate))
        })
        .with_context(|| {
            format!(
                "fresh YouTube manifest не содержит выбранную stream pair: {}",
                selected_stream.stream_id
            )
        })?;

    direct_streams_from_candidate(stream_candidates, selected_candidate)
}

/// Превращает selected service candidate в структуру, которую уже умеет открыть demux path.
fn direct_streams_from_candidate(
    stream_candidates: &YoutubeStreamCandidates,
    candidate: &YoutubeStreamCandidate,
) -> Result<YoutubeDirectStreams> {
    let audio = candidate.audio.clone().with_context(|| {
        format!(
            "selected YouTube stream {} не содержит audio companion descriptor",
            candidate.stream_id
        )
    })?;

    Ok(YoutubeDirectStreams {
        title: stream_candidates.title.clone(),
        service_media_id: stream_candidates
            .service_media_id
            .clone()
            .or_else(|| candidate.video.service_media_id.clone()),
        format_id: candidate.format_id.clone(),
        height: candidate.height,
        fps: candidate.fps,
        vcodec: candidate.vcodec.clone(),
        acodec: candidate.acodec.clone(),
        duration: stream_candidates.duration.or(candidate.video.duration),
        live: stream_candidates.live || candidate.video.live || audio.live,
        video: candidate.video.clone(),
        audio,
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

/// Проверяет, что format находится в текущем production envelope service open path-а.
fn is_supported_video_candidate_format(format: &YtDlpFormat) -> bool {
    is_video_only_format(format) && is_webm_format(format) && is_vp9_codec_tag(format)
}

/// Проверяет companion audio для текущего WebM dual-demux path-а.
fn is_supported_audio_companion_format(format: &YtDlpFormat) -> bool {
    is_audio_only_format(format) && is_webm_format(format) && is_opus_codec_tag(format)
}

/// Текущий service demux open path открывает только WebM byte streams.
fn is_webm_format(format: &YtDlpFormat) -> bool {
    format
        .ext
        .as_deref()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("webm"))
}

/// VP9 остаётся единственным YouTube video codec-ом в scope этой задачи.
fn is_vp9_codec_tag(format: &YtDlpFormat) -> bool {
    normalized_video_codec_tag(format)
        .as_deref()
        .is_some_and(|codec| codec.starts_with("vp9") || codec.starts_with("vp09"))
}

/// Opus/WebM сохраняет текущую audio policy legacy selector-а.
fn is_opus_codec_tag(format: &YtDlpFormat) -> bool {
    format
        .acodec
        .as_deref()
        .map(str::trim)
        .is_some_and(|codec| codec.eq_ignore_ascii_case("opus"))
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
        .filter(|format| is_supported_audio_companion_format(format))
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

    if codec_tag == "vp9" || codec_tag.starts_with("vp9.") || codec_tag.starts_with("vp09.") {
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
    }
}

/// Нормализует raw vcodec, отбрасывая `none` и пустые значения.
fn normalized_video_codec_tag(format: &YtDlpFormat) -> Option<String> {
    let codec_tag = format.vcodec.as_deref()?.trim();

    (!codec_tag.is_empty() && !codec_tag.eq_ignore_ascii_case("none"))
        .then(|| codec_tag.to_ascii_lowercase())
}

/// Строит VP9 requirement из простого `vp9` или ISO BMFF `vp09.*` tag.
fn vp9_requirement_from_codec_tag(
    codec_tag: &str,
    format: &YtDlpFormat,
) -> YoutubeVideoRequirement {
    let mut requirement = VideoDecodeRequirement::new(VideoCodec::Vp9);
    let mut missing_fields = Vec::new();

    if codec_tag == "vp9" {
        // yt-dlp на YouTube отдаёт SDR VP9 голым тегом `vp9` без ISO BMFF полей
        // profile/bit-depth/chroma. На YouTube такой stream всегда VP9 Profile 0
        // (8-bit 4:2:0 SDR) — это и есть базовый production decode path
        // (VP9 Profile 0 SDR -> VA-API -> NV12 -> WGPU SDR). HDR VP9 на YouTube
        // приходит только детальным тегом `vp09.02.*`; ошибочный HDR-намёк на
        // голом `vp9` дополнительно отсекает
        // reject_hdr_hint_without_resolved_hdr_metadata. Поэтому здесь это не
        // «угадывание», а доменный факт YouTube-адаптера.
        requirement = requirement
            .with_profile(VideoProfile::Vp9(Vp9Profile::Profile0))
            .with_bit_depth(BitDepth::Eight)
            .with_chroma(ChromaSubsampling::Yuv420)
            // Plain `vp9` — документированный YouTube SDR baseline. Typed SDR
            // fallback нужен selection boundary, чтобы manifest `SDR` не
            // превращался в ложный `UnknownDynamicRange`.
            .with_color(VideoColorMetadata::sdr_bt709_limited());
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
        url: crate::YoutubeDirectStreamUrl::from_secret_for_open(format.url),
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

    fn metadata_with_formats(formats: Vec<YtDlpFormat>) -> YtDlpMetadata {
        YtDlpMetadata {
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
            formats: Some(formats),
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
    fn plain_vp9_resolves_to_youtube_sdr_profile0_baseline() {
        let format = video_format("248", "vp9");

        let requirement = ready_requirement(&format);

        // YouTube отдаёт SDR VP9 голым тегом `vp9`; адаптер раскрывает его в
        // базовый production-профиль Profile 0 / 8-bit / 4:2:0 SDR, иначе
        // capability-aware startup path не получил бы ни одного ready candidate.
        assert_eq!(requirement.codec, VideoCodec::Vp9);
        assert_eq!(
            requirement.profile,
            Some(VideoProfile::Vp9(Vp9Profile::Profile0))
        );
        assert_eq!(requirement.bit_depth, Some(BitDepth::Eight));
        assert_eq!(requirement.chroma, Some(ChromaSubsampling::Yuv420));
        assert_eq!(
            requirement.color,
            Some(VideoColorMetadata::sdr_bt709_limited())
        );
        assert!(!requirement.hdr);
    }

    #[test]
    fn plain_vp9_hdr_hint_remains_unresolved_until_typed_selection() {
        // Plain `vp9` остаётся typed SDR baseline. Противоречащий manifest HDR
        // hint не получает HDR authority: selection увидит несогласованность и
        // вернёт UnknownDynamicRange.
        let mut format = video_format("248", "vp9");
        format.dynamic_range = Some("HDR".to_string());

        let requirement = video_requirement_from_format(&format);

        let YoutubeVideoRequirement::Ready(requirement) = requirement else {
            panic!("codec metadata должна сохраниться для typed selection rejection");
        };
        assert_eq!(youtube_dynamic_range(&format), YoutubeDynamicRange::Hdr);
        assert_eq!(
            requirement.color,
            Some(VideoColorMetadata::sdr_bt709_limited())
        );
        assert!(!requirement.requires_hdr_processing());
    }

    #[test]
    fn stream_candidates_preserve_descriptors_and_typed_metadata() {
        let mut video = video_format("315", "vp09.02.10.10.01.09.16.09.00");
        video.dynamic_range = Some("HDR".to_string());
        let metadata = metadata_with_formats(vec![
            video,
            audio_format("250", 70.0),
            audio_format("251", 160.0),
        ]);

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
        assert_eq!(candidate.height, Some(2160));
        assert_eq!(candidate.fps, Some(60.0));
        assert_eq!(
            candidate.vcodec.as_deref(),
            Some("vp09.02.10.10.01.09.16.09.00")
        );
        assert_eq!(candidate.acodec.as_deref(), Some("opus"));
        assert_eq!(candidate.dynamic_range, YoutubeDynamicRange::Hdr);
        let requirement = candidate
            .video_requirement
            .as_requirement()
            .expect("typed video requirement preserved");
        assert!(requirement.requires_hdr_processing());
    }

    #[test]
    fn stream_candidates_keep_current_webm_vp9_opus_scope() {
        let mut av1_mp4_video = video_format("399", "av01.0.08M.08");
        av1_mp4_video.ext = Some("mp4".to_string());
        let mut h264_mp4_video = video_format("137", "avc1.640028");
        h264_mp4_video.ext = Some("mp4".to_string());
        let mut high_bitrate_m4a_audio = audio_format("140", 320.0);
        high_bitrate_m4a_audio.ext = Some("m4a".to_string());
        high_bitrate_m4a_audio.acodec = Some("mp4a.40.2".to_string());
        let metadata = metadata_with_formats(vec![
            av1_mp4_video,
            h264_mp4_video,
            video_format("315", "vp09.02.10.10.01.09.16.09.00"),
            high_bitrate_m4a_audio,
            audio_format("251", 160.0),
        ]);

        let candidates = build_stream_candidates(&metadata).expect("candidates built");

        assert_eq!(candidates.candidates.len(), 1);
        let candidate = &candidates.candidates[0];
        assert_eq!(candidate.stream_id, "abc123:v315:a251");
        assert_eq!(candidate.video.format_id.as_deref(), Some("315"));
        assert_eq!(
            candidate
                .audio
                .as_ref()
                .and_then(|audio| audio.format_id.as_deref()),
            Some("251")
        );
    }

    #[test]
    fn selected_candidate_refresh_match_preserves_chosen_format_identity() {
        let initial_candidates = build_stream_candidates(&metadata_with_formats(vec![
            video_format("315", "vp09.02.10.10.01.09.16.09.00"),
            audio_format("251", 160.0),
        ]))
        .expect("initial candidates built");
        let selected_identity =
            YoutubeSelectedStreamIdentity::from_candidate(&initial_candidates.candidates[0]);
        let mut refreshed_candidates = initial_candidates.clone();
        refreshed_candidates.candidates[0].stream_id =
            "fresh-manifest-renumbered:v315:a251".to_string();

        let direct_streams =
            direct_streams_from_matching_candidate(&refreshed_candidates, &selected_identity)
                .expect("format identity fallback selects refreshed candidate");

        assert_eq!(direct_streams.format_id.as_deref(), Some("315+251"));
        assert_eq!(direct_streams.video.format_id.as_deref(), Some("315"));
        assert_eq!(direct_streams.audio.format_id.as_deref(), Some("251"));
    }
}
