use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use capability_core::{SelectedVideoStream, StreamSelectionError, SystemCapabilities};
use player_core::PreparedMedia;
use rustiplayer_config::{AppConfig, NetworkConfig, PlayerDemuxConfig, YoutubeConfig};
use service_youtube::YoutubeStreamCandidates;
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::state::AppState;

/// Интервал polling-а фоновой подготовки startup media, когда playback ещё не активен.
pub(crate) const STARTUP_MEDIA_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Media, которое нужно автоматически открыть после создания окна.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InitialMedia {
    /// Локальный файл.
    File(PathBuf),

    /// YouTube URL, который нужно подготовить через service-specific resolver.
    YouTubeUrl { url: String },

    /// Обычный seekable `http(s)` media URL с явным container extension.
    DirectMediaUrl { url: String },
}

/// Подготовленный YouTube playback source и compact identity выбранной stream-пары.
pub(crate) struct PreparedYoutubeStartupMedia {
    /// Уже открытый demuxer для текущего playback open.
    pub(crate) streaming_media: service_youtube::YoutubeStreamingMedia,

    /// Минимальная identity, по которой повторное открытие может восстановить тот же выбор.
    pub(crate) selected_stream_identity: service_youtube::YoutubeSelectedStreamIdentity,
}

/// Результат фоновой подготовки CLI YouTube URL.
type YoutubeStartupResult = std::result::Result<PreparedYoutubeStartupMedia, String>;

/// Результат фоновой подготовки generic direct media URL.
type DirectMediaStartupResult =
    std::result::Result<service_direct_media::DirectMediaOpenResult, String>;

/// Фоновый job, который не блокирует создание окна и UI.
struct YoutubeStartupJob {
    /// URL страницы/ролика, который был передан через CLI.
    source_url: String,

    /// Текст pending-состояния для центрального overlay.
    pending_message: String,

    /// Receiver одноразового результата background resolver-а.
    result_rx: Receiver<YoutubeStartupResult>,

    /// JoinHandle нужен для cleanup после получения результата.
    join_handle: Option<JoinHandle<()>>,
}

impl YoutubeStartupJob {
    /// Запускает подготовку YouTube media на отдельном thread-е.
    fn spawn(
        source_url: String,
        app_config: AppConfig,
        system_capabilities: SystemCapabilities,
    ) -> std::result::Result<Self, String> {
        let (result_tx, result_rx) = mpsc::channel();
        let thread_url = source_url.clone();
        let network_config = app_config.network.clone();
        let youtube_config = app_config.youtube.clone();
        let demux_config = app_config.player.demux;
        let join_handle = thread::Builder::new()
            .name("youtube-startup-resolver".to_string())
            .spawn(move || {
                let resolve_result = resolve_youtube_startup_media(
                    &thread_url,
                    &network_config,
                    &youtube_config,
                    &demux_config,
                    &system_capabilities,
                )
                .map_err(|error| format!("{error:#}"));

                if result_tx.send(resolve_result).is_err() {
                    debug!("UI больше не ждёт результат YouTube startup resolver-а");
                }
            })
            .map_err(|error| format!("Не удалось запустить YouTube startup resolver: {error}"))?;

        Ok(Self {
            source_url,
            pending_message: "Подготовка YouTube stream...".to_string(),
            result_rx,
            join_handle: Some(join_handle),
        })
    }

    /// Возвращает pending-текст без доступа к внутреннему channel state.
    fn pending_message(&self) -> &str {
        &self.pending_message
    }

    /// Неблокирующе забирает результат resolver-а, если он уже готов.
    fn try_take_result(&mut self) -> Option<YoutubeStartupResult> {
        let result = match self.result_rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err("YouTube startup resolver завершился без результата".to_string())
            }
        };

        if let Some(join_handle) = self.join_handle.take()
            && join_handle.join().is_err()
        {
            return Some(Err(
                "YouTube startup resolver завершился panic после результата".to_string(),
            ));
        }

        Some(result)
    }
}

/// Фоновый job для generic direct media URL.
struct DirectMediaStartupJob {
    /// Direct media URL, который был передан через CLI.
    source_url: String,

    /// Текст pending-состояния для центрального overlay.
    pending_message: String,

    /// Receiver одноразового результата background opener-а.
    result_rx: Receiver<DirectMediaStartupResult>,

    /// JoinHandle нужен для cleanup после получения результата.
    join_handle: Option<JoinHandle<()>>,
}

impl DirectMediaStartupJob {
    /// Запускает подготовку direct media на отдельном thread-е.
    fn spawn(source_url: String, app_config: AppConfig) -> std::result::Result<Self, String> {
        let (result_tx, result_rx) = mpsc::channel();
        let thread_url = source_url.clone();
        let network_config = app_config.network.clone();
        let demux_config = app_config.player.demux;
        let join_handle = thread::Builder::new()
            .name("direct-media-startup-opener".to_string())
            .spawn(move || {
                let open_result =
                    resolve_direct_media_startup_media(&thread_url, &network_config, &demux_config)
                        .map_err(|error| format!("{error:#}"));

                if result_tx.send(open_result).is_err() {
                    debug!("UI больше не ждёт результат direct media startup opener-а");
                }
            })
            .map_err(|error| {
                format!("Не удалось запустить direct media startup opener: {error}")
            })?;

        Ok(Self {
            source_url,
            pending_message: "Подготовка direct media URL...".to_string(),
            result_rx,
            join_handle: Some(join_handle),
        })
    }

    /// Возвращает pending-текст без доступа к внутреннему channel state.
    fn pending_message(&self) -> &str {
        &self.pending_message
    }

    /// Неблокирующе забирает результат opener-а, если он уже готов.
    fn try_take_result(&mut self) -> Option<DirectMediaStartupResult> {
        let result = match self.result_rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err("Direct media startup opener завершился без результата".to_string())
            }
        };

        if let Some(join_handle) = self.join_handle.take()
            && join_handle.join().is_err()
        {
            return Some(Err(
                "Direct media startup opener завершился panic после результата".to_string(),
            ));
        }

        Some(result)
    }
}

/// Владеет shell-состоянием стартового media без знания о renderer/GPU.
pub(crate) struct StartupMediaController {
    /// Media, переданное через CLI или восстановленное после suspend.
    initial_media: Option<InitialMedia>,

    /// Фоновая подготовка CLI YouTube URL, если она уже запущена.
    youtube_startup_job: Option<YoutubeStartupJob>,

    /// Фоновая подготовка generic direct media URL, если она уже запущена.
    direct_media_startup_job: Option<DirectMediaStartupJob>,

    /// Startup-ошибка shell-слоя, которую нужно показать после создания UI.
    startup_error: Option<String>,
}

impl StartupMediaController {
    /// Создаёт controller из startup-состояния, которое было собрано до запуска окна.
    pub(crate) fn new(initial_media: Option<InitialMedia>, startup_error: Option<String>) -> Self {
        Self {
            initial_media,
            youtube_startup_job: None,
            direct_media_startup_job: None,
            startup_error,
        }
    }

    /// Возвращает копию startup-ошибки для инициализации `AppState`.
    pub(crate) fn startup_error_message(&self) -> Option<String> {
        self.startup_error.clone()
    }

    /// Возвращает pending-текст активного startup job.
    pub(crate) fn pending_message(&self) -> Option<&str> {
        self.youtube_startup_job
            .as_ref()
            .map(YoutubeStartupJob::pending_message)
            .or_else(|| {
                self.direct_media_startup_job
                    .as_ref()
                    .map(DirectMediaStartupJob::pending_message)
            })
    }

    /// Сообщает shell scheduler-у, нужно ли продолжать polling startup jobs.
    pub(crate) fn has_pending_startup_job(&self) -> bool {
        self.youtube_startup_job.is_some() || self.direct_media_startup_job.is_some()
    }

    /// Запоминает текущий локальный файл для повторного открытия после следующего resume.
    pub(crate) fn restore_file_on_next_resume(&mut self, path: &Path) {
        self.initial_media = Some(InitialMedia::File(path.to_path_buf()));
        self.startup_error = None;
    }

    /// Запускает отложенное стартовое media после того, как `AppState` уже создан.
    pub(crate) fn start_pending_initial_media(
        &mut self,
        app_state: &mut AppState,
        app_config: &AppConfig,
        system_capabilities: &SystemCapabilities,
    ) {
        if let Some(pending_message) = self.pending_message() {
            app_state.set_startup_pending(pending_message.to_string());
        }

        let Some(initial_media) = self.initial_media.take() else {
            return;
        };

        match initial_media {
            InitialMedia::File(path) => {
                info!(path = %path.display(), "Автозагрузка файла из CLI");
                app_state.load_file(&path);
            }
            InitialMedia::YouTubeUrl { url } => {
                info!(source = %url, "Автозагрузка YouTube URL из CLI");
                self.start_youtube_startup_job(url, app_state, app_config, system_capabilities);
            }
            InitialMedia::DirectMediaUrl { url } => {
                info!(source = %url, "Автозагрузка direct media URL из CLI");
                self.start_direct_media_startup_job(url, app_state, app_config);
            }
        }
    }

    /// Неблокирующе опрашивает результаты фоновой подготовки startup URL-ов.
    pub(crate) fn poll_startup_jobs(&mut self, app_state: &mut AppState) {
        self.poll_youtube_job(app_state);
        poll_direct_media_startup_job(
            &mut self.direct_media_startup_job,
            app_state,
            &mut self.startup_error,
        );
    }

    /// Неблокирующе опрашивает результат фоновой подготовки YouTube URL.
    pub(crate) fn poll_youtube_job(&mut self, app_state: &mut AppState) {
        poll_youtube_startup_job(
            &mut self.youtube_startup_job,
            app_state,
            &mut self.startup_error,
        );
    }

    /// Запускает background resolve для CLI YouTube URL и сразу обновляет UI state.
    fn start_youtube_startup_job(
        &mut self,
        source_url: String,
        app_state: &mut AppState,
        app_config: &AppConfig,
        system_capabilities: &SystemCapabilities,
    ) {
        app_state.set_startup_pending("Подготовка YouTube stream...".to_string());
        match YoutubeStartupJob::spawn(source_url, app_config.clone(), system_capabilities.clone())
        {
            Ok(job) => {
                self.startup_error = None;
                self.youtube_startup_job = Some(job);
            }
            Err(error) => {
                warn!(error = %error, "Не удалось запустить YouTube startup resolver");
                let startup_error = format!("NetworkError: YouTube error: {error}");
                self.startup_error = Some(startup_error.clone());
                app_state.set_startup_error(startup_error);
            }
        }
    }

    /// Запускает background open для CLI direct media URL и сразу обновляет UI state.
    fn start_direct_media_startup_job(
        &mut self,
        source_url: String,
        app_state: &mut AppState,
        app_config: &AppConfig,
    ) {
        app_state.set_startup_pending("Подготовка direct media URL...".to_string());
        match DirectMediaStartupJob::spawn(source_url, app_config.clone()) {
            Ok(job) => {
                self.startup_error = None;
                self.direct_media_startup_job = Some(job);
            }
            Err(error) => {
                warn!(error = %error, "Не удалось запустить direct media startup opener");
                let startup_error = format!("NetworkError: Direct media error: {error}");
                self.startup_error = Some(startup_error.clone());
                app_state.set_startup_error(startup_error);
            }
        }
    }
}

/// Ошибка выбора YouTube stream-а до открытия demuxer-а.
#[derive(Debug, Error)]
enum YoutubeStartupSelectionError {
    /// Service candidates есть, но ни один не содержит полной metadata для capability-core.
    #[error("YouTube stream candidates не содержат ready video metadata: {0}")]
    NoReadyVideoCandidates(String),

    /// Capability-core отклонил все ready candidates с typed причиной.
    #[error("YouTube stream rejected by current hardware/render capabilities: {0}")]
    Capability(#[from] StreamSelectionError),
}

/// Выполняет production chain: service candidates -> capability selection -> demux open.
pub(crate) fn resolve_youtube_startup_media(
    source_url: &str,
    network_config: &NetworkConfig,
    youtube_config: &YoutubeConfig,
    demux_config: &PlayerDemuxConfig,
    system_capabilities: &SystemCapabilities,
) -> Result<PreparedYoutubeStartupMedia> {
    let stream_candidates =
        service_youtube::resolve_youtube_stream_candidates_with_config(source_url, youtube_config)
            .context("Не удалось получить YouTube stream candidates")?;
    let selected_stream = select_youtube_startup_candidate(&stream_candidates, system_capabilities)
        .context("Не удалось выбрать YouTube stream по system capabilities")?;

    let selected_stream_identity = service_youtube::YoutubeSelectedStreamIdentity::from_candidates(
        &stream_candidates,
        &selected_stream.stream_id,
    )
    .context("Не удалось сохранить выбранную YouTube stream identity")?;
    let streaming_media = service_youtube::open_streaming_media_from_candidates_with_demux_config(
        source_url,
        &stream_candidates,
        &selected_stream.stream_id,
        network_config,
        youtube_config,
        demux_config,
    )
    .with_context(|| {
        format!(
            "Не удалось открыть выбранный YouTube stream {}",
            selected_stream.stream_id
        )
    })?;

    Ok(PreparedYoutubeStartupMedia {
        streaming_media,
        selected_stream_identity,
    })
}

/// Выполняет generic direct media open chain без YouTube-specific semantics.
pub(crate) fn resolve_direct_media_startup_media(
    source_url: &str,
    network_config: &NetworkConfig,
    demux_config: &PlayerDemuxConfig,
) -> Result<service_direct_media::DirectMediaOpenResult> {
    service_direct_media::open_direct_media_url(source_url, network_config, demux_config)
        .context("Не удалось открыть direct media URL")
}

/// Временная политика: HDR-потоки YouTube не выбираются, пока в UI нет
/// переключателя HDR.
///
/// HDR decode/render path (VP9 Profile 2 -> P010 -> BT.2446-C HDR→SDR) уже
/// реализован и сам gated в `capability-core`/`render-wgpu-video`. Когда в UI
/// появится кнопка, этот `const` станет runtime-значением из UI/config, и
/// HDR-кандидаты (детальный тег `vp09.02.*`) станут выбираемыми без других
/// изменений в этом модуле.
const ALLOW_YOUTUBE_HDR: bool = false;

/// Сообщает, что ready requirement кандидата требует HDR-обработки.
///
/// Использует намерение `VideoDecodeRequirement::requires_hdr_processing`, а не
/// читает поля `hdr`/`color` напрямую. Insufficient-кандидаты HDR не требуют
/// (их и так нельзя выбрать), поэтому возвращаем `false`.
fn youtube_candidate_requires_hdr(candidate: &service_youtube::YoutubeStreamCandidate) -> bool {
    candidate
        .video_requirement
        .as_requirement()
        .is_some_and(|requirement| requirement.requires_hdr_processing())
}

/// Выбирает лучший YouTube candidate строго до открытия media bytes.
fn select_youtube_startup_candidate(
    stream_candidates: &YoutubeStreamCandidates,
    system_capabilities: &SystemCapabilities,
) -> std::result::Result<SelectedVideoStream, YoutubeStartupSelectionError> {
    let capability_candidates = stream_candidates
        .candidates
        .iter()
        .filter(|candidate| candidate.audio.is_some())
        // ВРЕМЕННО: пока в UI нет HDR-переключателя, отбираем только SDR-потоки;
        // HDR-кандидаты исключаются ещё до capability selection.
        .filter(|candidate| ALLOW_YOUTUBE_HDR || !youtube_candidate_requires_hdr(candidate))
        .filter_map(service_youtube::YoutubeStreamCandidate::to_capability_candidate)
        .collect::<Vec<_>>();

    if capability_candidates.is_empty() {
        return Err(YoutubeStartupSelectionError::NoReadyVideoCandidates(
            youtube_no_ready_candidates_reason(stream_candidates),
        ));
    }

    Ok(system_capabilities.select_best_video_stream(&capability_candidates)?)
}

/// Формирует короткую причину, почему service candidates нельзя передать в capability-core.
fn youtube_no_ready_candidates_reason(stream_candidates: &YoutubeStreamCandidates) -> String {
    if stream_candidates.candidates.is_empty() {
        return "yt-dlp не вернул WebM VP9/Opus candidates для текущего production path"
            .to_string();
    }

    let reasons = stream_candidates
        .candidates
        .iter()
        .filter_map(|candidate| {
            if candidate.audio.is_none() {
                return Some(format!(
                    "{}: отсутствует WebM/Opus audio companion",
                    candidate.stream_id
                ));
            }

            candidate
                .video_requirement
                .insufficient_reason()
                .map(|reason| format!("{}: {reason}", candidate.stream_id))
        })
        .take(3)
        .collect::<Vec<_>>();

    if reasons.is_empty() {
        return "все candidates были отфильтрованы до capability selection".to_string();
    }

    reasons.join("; ")
}

/// Забирает готовый результат фоновой подготовки YouTube и доставляет его в UI/player.
fn poll_youtube_startup_job(
    job_slot: &mut Option<YoutubeStartupJob>,
    app_state: &mut AppState,
    startup_error_slot: &mut Option<String>,
) {
    let Some(job) = job_slot.as_mut() else {
        return;
    };
    let Some(resolve_result) = job.try_take_result() else {
        return;
    };
    let source_url = job.source_url.clone();
    *job_slot = None;

    match resolve_result {
        Ok(prepared_youtube_media) => {
            *startup_error_slot = None;
            info!(
                source = %source_url,
                description = %prepared_youtube_media.streaming_media.description,
                "YouTube media подготовлен для streaming playback"
            );
            app_state.load_youtube_demuxer(
                source_url,
                prepared_youtube_media.streaming_media.description,
                prepared_youtube_media.streaming_media.demuxer,
                prepared_youtube_media.selected_stream_identity,
            );
        }
        Err(error) => {
            warn!(source = %source_url, error = %error, "Не удалось подготовить YouTube URL");
            let startup_error = format!("NetworkError: YouTube error: {error}");
            *startup_error_slot = Some(startup_error.clone());
            app_state.set_startup_error(startup_error);
        }
    }
}

/// Забирает готовый результат фоновой подготовки direct media и доставляет его в UI/player.
fn poll_direct_media_startup_job(
    job_slot: &mut Option<DirectMediaStartupJob>,
    app_state: &mut AppState,
    startup_error_slot: &mut Option<String>,
) {
    let Some(job) = job_slot.as_mut() else {
        return;
    };
    let Some(open_result) = job.try_take_result() else {
        return;
    };
    let source_url = job.source_url.clone();
    *job_slot = None;

    match open_result {
        Ok(opened_media) => {
            *startup_error_slot = None;
            let source_label = opened_media.source_label().to_string();
            let prepared_media = PreparedMedia::from_external_label(
                source_label.clone(),
                opened_media.into_demuxer(),
            );
            info!(
                source = %source_url,
                label = %source_label,
                "Direct media URL подготовлен для playback"
            );
            app_state.load_prepared_direct_media(source_url, source_label, prepared_media);
        }
        Err(error) => {
            warn!(source = %source_url, error = %error, "Не удалось подготовить direct media URL");
            let startup_error = format!("NetworkError: Direct media error: {error}");
            *startup_error_slot = Some(startup_error.clone());
            app_state.set_startup_error(startup_error);
        }
    }
}

/// Подготавливает стартовый media-файл из CLI-аргумента.
///
/// Локальный путь возвращается как файл. Web URL маршрутизируется либо в
/// YouTube adapter, либо в generic direct media opener.
pub(crate) fn resolve_initial_media_from_cli(
    app_config: &AppConfig,
) -> (Option<InitialMedia>, Option<String>) {
    // Берём только первый пользовательский аргумент, чтобы не вводить неполный CLI parser.
    let Some(argument) = std::env::args().nth(1) else {
        return (None, None);
    };

    resolve_initial_media_argument(argument, app_config)
}

/// Pure helper для тестируемой маршрутизации одного CLI аргумента.
fn resolve_initial_media_argument(
    argument: String,
    app_config: &AppConfig,
) -> (Option<InitialMedia>, Option<String>) {
    if service_youtube::is_supported_youtube_url(&argument) {
        info!(url = %argument, "CLI аргумент распознан как YouTube URL");
        if !app_config.youtube.enabled {
            return (
                None,
                Some("NetworkError: YouTube adapter отключён в config".to_string()),
            );
        }

        return (Some(InitialMedia::YouTubeUrl { url: argument }), None);
    }

    if service_youtube::is_probably_url(&argument)
        || service_direct_media::looks_like_url(&argument)
    {
        return resolve_direct_media_initial_url(argument);
    }

    // Всё остальное считаем локальным путём, как работало раньше.
    (Some(InitialMedia::File(PathBuf::from(argument))), None)
}

/// Классифицирует direct media URL без сетевых запросов и возвращает typed startup error.
fn resolve_direct_media_initial_url(argument: String) -> (Option<InitialMedia>, Option<String>) {
    match service_direct_media::parse_direct_media_url(&argument) {
        Ok(direct_url) => (
            Some(InitialMedia::DirectMediaUrl {
                url: direct_url.url().to_string(),
            }),
            None,
        ),
        Err(error) => (
            None,
            Some(format!(
                "NetworkError: Direct media URL unsupported: {error}"
            )),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::Duration;

    use capability_core::{
        BackendCapabilities, BackendDriverInfo, BackendProbeStatus, SupportedVideoOutput,
        SystemCapabilities,
    };
    use codec_core::{
        BitDepth, ChromaSubsampling, DecodeBackendId, SupportedVideoDecodeFormat, VideoCodec,
        VideoDecodeRequirement, VideoProfile, Vp9Profile,
    };
    use render_core::RenderCapabilities;
    use service_youtube::{
        YoutubeDirectStreamDescriptor, YoutubeInsufficientVideoMetadata, YoutubeStreamCandidate,
        YoutubeStreamCandidates, YoutubeStreamKind, YoutubeVideoRequirement,
    };
    use source_core::SourceValidators;
    use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

    use super::*;

    fn capabilities_with_vp9_profile0() -> SystemCapabilities {
        let decode_format = SupportedVideoDecodeFormat {
            codec: VideoCodec::Vp9,
            profile: VideoProfile::Vp9(Vp9Profile::Profile0),
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            max_width: Some(4096),
            max_height: Some(2304),
            max_fps: None,
            hdr_input: false,
        };
        let output = SupportedVideoOutput {
            backend: DecodeBackendId::vaapi(),
            decode_format,
            frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        };

        SystemCapabilities {
            schema_version: capability_core::CURRENT_CAPABILITY_SCHEMA_VERSION,
            probed_at_unix_seconds: 1,
            video_backends: vec![BackendCapabilities {
                backend_id: DecodeBackendId::vaapi(),
                display_name: "VA-API".to_string(),
                status: BackendProbeStatus::Available,
                driver: BackendDriverInfo::default(),
                raw_supported_outputs: vec![output.clone()],
                raw_profiles: Vec::new(),
                raw_entrypoints: Vec::new(),
                raw_rt_formats: Vec::new(),
                quirks: Vec::new(),
                diagnostics: Vec::new(),
            }],
            render_backends: vec![RenderCapabilities::wgpu_nv12(Some(4096))],
            playable_video_outputs: vec![output],
        }
    }

    fn youtube_descriptor(
        kind: YoutubeStreamKind,
        format_id: &str,
    ) -> YoutubeDirectStreamDescriptor {
        let kind_label = match kind {
            YoutubeStreamKind::Video => "video",
            YoutubeStreamKind::Audio => "audio",
        };

        YoutubeDirectStreamDescriptor {
            kind,
            url: format!("http://media.test/{format_id}.webm"),
            headers: Vec::new(),
            format_id: Some(format_id.to_string()),
            service_media_id: Some("media-id".to_string()),
            validators: SourceValidators::default(),
            duration: Some(Duration::from_secs(120)),
            live: false,
            description: format!("{kind_label} descriptor"),
        }
    }

    fn vp9_requirement(profile: Vp9Profile, bit_depth: BitDepth) -> VideoDecodeRequirement {
        VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_profile(VideoProfile::Vp9(profile))
            .with_bit_depth(bit_depth)
            .with_chroma(ChromaSubsampling::Yuv420)
            .with_resolution(1920, 1080)
            .with_frame_rate(60.0)
    }

    fn ready_youtube_candidate(
        stream_id: &str,
        format_id: &str,
        requirement: VideoDecodeRequirement,
        quality_score: i64,
    ) -> YoutubeStreamCandidate {
        YoutubeStreamCandidate {
            stream_id: stream_id.to_string(),
            format_id: Some(format!("{format_id}+251")),
            video: youtube_descriptor(YoutubeStreamKind::Video, format_id),
            audio: Some(youtube_descriptor(YoutubeStreamKind::Audio, "251")),
            height: requirement.height,
            fps: requirement.fps,
            vcodec: Some("vp09.00.10.08".to_string()),
            acodec: Some("opus".to_string()),
            video_requirement: YoutubeVideoRequirement::Ready(requirement),
            quality_score,
        }
    }

    fn youtube_candidates(candidates: Vec<YoutubeStreamCandidate>) -> YoutubeStreamCandidates {
        YoutubeStreamCandidates {
            title: Some("sample".to_string()),
            service_media_id: Some("media-id".to_string()),
            duration: Some(Duration::from_secs(120)),
            live: false,
            candidates,
        }
    }

    #[test]
    fn controller_exposes_startup_error_for_app_state_creation() {
        let controller = StartupMediaController::new(None, Some("startup failure".to_string()));

        assert_eq!(
            controller.startup_error_message(),
            Some("startup failure".to_string())
        );
    }

    #[test]
    fn restore_file_on_next_resume_replaces_initial_media_and_clears_error() {
        let restored_path = Path::new("/tmp/restored.webm");
        let mut controller = StartupMediaController::new(
            Some(InitialMedia::YouTubeUrl {
                url: "https://example.test/video".to_string(),
            }),
            Some("old error".to_string()),
        );

        controller.restore_file_on_next_resume(restored_path);

        assert!(controller.startup_error_message().is_none());
        assert!(matches!(
            controller.initial_media.as_ref(),
            Some(InitialMedia::File(path)) if path.as_path() == restored_path
        ));
    }

    #[test]
    fn pending_message_reports_existing_youtube_job() {
        let (_result_tx, result_rx) = mpsc::channel();
        let controller = StartupMediaController {
            initial_media: None,
            youtube_startup_job: Some(YoutubeStartupJob {
                source_url: "https://example.test/video".to_string(),
                pending_message: "Подготовка YouTube stream...".to_string(),
                result_rx,
                join_handle: None,
            }),
            direct_media_startup_job: None,
            startup_error: None,
        };

        assert!(controller.has_pending_startup_job());
        assert_eq!(
            controller.pending_message(),
            Some("Подготовка YouTube stream...")
        );
    }

    #[test]
    fn cli_route_keeps_local_path_unchanged() {
        let (initial_media, startup_error) =
            resolve_initial_media_argument("/tmp/sample.mp4".to_string(), &AppConfig::default());

        assert!(startup_error.is_none());
        assert!(matches!(
            initial_media,
            Some(InitialMedia::File(path)) if path == Path::new("/tmp/sample.mp4")
        ));
    }

    #[test]
    fn cli_route_sends_youtube_host_to_youtube_path() {
        let (initial_media, startup_error) = resolve_initial_media_argument(
            "https://youtu.be/video-id".to_string(),
            &AppConfig::default(),
        );

        assert!(startup_error.is_none());
        assert!(matches!(
            initial_media,
            Some(InitialMedia::YouTubeUrl { url }) if url == "https://youtu.be/video-id"
        ));
    }

    #[test]
    fn cli_route_sends_supported_http_media_to_direct_path() {
        let (initial_media, startup_error) = resolve_initial_media_argument(
            "https://cdn.example.test/video.mp4?token=1".to_string(),
            &AppConfig::default(),
        );

        assert!(startup_error.is_none());
        assert!(matches!(
            initial_media,
            Some(InitialMedia::DirectMediaUrl { url })
                if url == "https://cdn.example.test/video.mp4?token=1"
        ));
    }

    #[test]
    fn cli_route_sends_quicktime_mov_http_media_to_direct_path() {
        let (initial_media, startup_error) = resolve_initial_media_argument(
            "https://cdn.example.test/camera/ios-hevc-main10-aac-4k60.MOV".to_string(),
            &AppConfig::default(),
        );

        assert!(startup_error.is_none());
        assert!(matches!(
            initial_media,
            Some(InitialMedia::DirectMediaUrl { url })
                if url == "https://cdn.example.test/camera/ios-hevc-main10-aac-4k60.MOV"
        ));
    }

    #[test]
    fn cli_route_rejects_http_media_without_supported_extension() {
        let (initial_media, startup_error) = resolve_initial_media_argument(
            "https://192.0.2.10/media".to_string(),
            &AppConfig::default(),
        );

        assert!(initial_media.is_none());
        assert!(
            startup_error
                .as_deref()
                .is_some_and(|error| error.contains("Direct media URL unsupported"))
        );
    }

    #[test]
    fn cli_route_rejects_unsupported_media_protocol() {
        let (initial_media, startup_error) = resolve_initial_media_argument(
            "rtsp://192.0.2.10/video.mp4".to_string(),
            &AppConfig::default(),
        );

        assert!(initial_media.is_none());
        assert!(
            startup_error
                .as_deref()
                .is_some_and(|error| error.contains("protocol `rtsp`"))
        );
    }

    #[test]
    fn youtube_startup_selection_picks_supported_candidate_before_demux_open() {
        let candidates = youtube_candidates(vec![
            ready_youtube_candidate(
                "hdr-p010",
                "315",
                vp9_requirement(Vp9Profile::Profile2, BitDepth::Ten),
                10_000,
            ),
            ready_youtube_candidate(
                "sdr-nv12",
                "303",
                vp9_requirement(Vp9Profile::Profile0, BitDepth::Eight),
                1_000,
            ),
        ]);

        let selected =
            select_youtube_startup_candidate(&candidates, &capabilities_with_vp9_profile0())
                .expect("supported SDR candidate is selected");

        assert_eq!(selected.stream_id, "sdr-nv12");
    }

    #[test]
    fn youtube_startup_selection_returns_typed_capability_rejection() {
        let candidates = youtube_candidates(vec![ready_youtube_candidate(
            "sdr-nv12",
            "303",
            vp9_requirement(Vp9Profile::Profile0, BitDepth::Eight),
            1_000,
        )]);

        let error = select_youtube_startup_candidate(&candidates, &SystemCapabilities::empty(1))
            .expect_err("empty capabilities reject ready candidate");

        assert!(matches!(
            error,
            YoutubeStartupSelectionError::Capability(StreamSelectionError::Unsupported(_))
        ));
    }

    #[test]
    fn youtube_startup_selection_reports_insufficient_service_metadata() {
        let mut candidate = ready_youtube_candidate(
            "plain-vp9",
            "248",
            VideoDecodeRequirement::new(VideoCodec::Vp9),
            1_000,
        );
        candidate.video_requirement =
            YoutubeVideoRequirement::Insufficient(YoutubeInsufficientVideoMetadata {
                reason: "missing VP9 profile metadata".to_string(),
                partial_requirement: Some(VideoDecodeRequirement::new(VideoCodec::Vp9)),
            });
        let candidates = youtube_candidates(vec![candidate]);

        let error =
            select_youtube_startup_candidate(&candidates, &capabilities_with_vp9_profile0())
                .expect_err("insufficient service metadata is rejected before selection");

        assert!(matches!(
            error,
            YoutubeStartupSelectionError::NoReadyVideoCandidates(reason)
                if reason.contains("plain-vp9") && reason.contains("missing VP9 profile metadata")
        ));
    }

    #[test]
    fn youtube_startup_selection_rejects_candidate_without_audio_companion() {
        let mut candidate = ready_youtube_candidate(
            "video-without-audio",
            "303",
            vp9_requirement(Vp9Profile::Profile0, BitDepth::Eight),
            1_000,
        );
        candidate.audio = None;
        let candidates = youtube_candidates(vec![candidate]);

        let error =
            select_youtube_startup_candidate(&candidates, &capabilities_with_vp9_profile0())
                .expect_err("video-only candidate is not openable by current service path");

        assert!(matches!(
            error,
            YoutubeStartupSelectionError::NoReadyVideoCandidates(reason)
                if reason.contains("video-without-audio")
                    && reason.contains("audio companion")
        ));
    }

    #[test]
    fn youtube_startup_selection_temporarily_skips_hdr_candidate() {
        // HDR-кандидат с самым высоким quality_score: без временного фильтра он
        // был бы предпочтён, но пока в UI нет HDR-переключателя выбираем SDR.
        let mut hdr_requirement = vp9_requirement(Vp9Profile::Profile2, BitDepth::Ten);
        hdr_requirement.hdr = true;
        let candidates = youtube_candidates(vec![
            ready_youtube_candidate("hdr-stream", "315", hdr_requirement, 10_000),
            ready_youtube_candidate(
                "sdr-nv12",
                "303",
                vp9_requirement(Vp9Profile::Profile0, BitDepth::Eight),
                1_000,
            ),
        ]);

        let selected =
            select_youtube_startup_candidate(&candidates, &capabilities_with_vp9_profile0())
                .expect("SDR candidate остаётся выбираемым при отключённом HDR");

        assert_eq!(selected.stream_id, "sdr-nv12");
    }

    #[test]
    fn youtube_startup_hdr_only_is_filtered_before_capability_selection() {
        // Единственный кандидат — HDR. Временный фильтр убирает его ещё до
        // capability selection, поэтому ошибка именно NoReadyVideoCandidates
        // (отфильтрован), а не типизированный Capability rejection.
        let mut hdr_requirement = vp9_requirement(Vp9Profile::Profile2, BitDepth::Ten);
        hdr_requirement.hdr = true;
        let candidates = youtube_candidates(vec![ready_youtube_candidate(
            "hdr-only",
            "315",
            hdr_requirement,
            10_000,
        )]);

        let error =
            select_youtube_startup_candidate(&candidates, &capabilities_with_vp9_profile0())
                .expect_err("HDR-only кандидат отфильтрован до capability selection");

        assert!(matches!(
            error,
            YoutubeStartupSelectionError::NoReadyVideoCandidates(_)
        ));
    }
}
