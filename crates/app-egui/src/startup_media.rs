use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use capability_core::{SelectedVideoStream, SystemCapabilities};
use codec_core::VideoCodec as RuntimeVideoCodec;
use rustiplayer_config::{
    AppConfig, NetworkConfig, PlayerDemuxConfig, VideoCodec as ConfigVideoCodec, YtDlpConfig,
    YtDlpHdrSelection,
};
use service_ytdlp::YtDlpStreamCandidates;
use tracing::{debug, info, warn};

#[cfg(test)]
use crate::app_wake::AppWakeOwner;
use crate::app_wake::{
    AppWakePort, CompletionPublishError, OwnerMailboxReceiver, WakeDelivery, owner_mailbox,
};
use crate::process_shutdown::{FinishedThreadJoin, join_finished_thread};
use crate::state::AppState;
use crate::url_service_adapter::{
    StartupUrlClassification, StartupUrlLocator, classify_startup_url,
};

mod orchestration;
mod pending_install;
mod shutdown;

pub(crate) use orchestration::StartupMediaPhase;
use orchestration::{StartupMediaOrchestration, StartupMediaTarget};

/// Интервал polling-а фоновой подготовки startup media, когда playback ещё не активен.
pub(crate) const STARTUP_MEDIA_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Media, которое нужно автоматически открыть после создания окна.
#[derive(Debug)]
pub(crate) enum InitialMedia {
    /// Локальный файл.
    File(PathBuf),

    /// URL, уже классифицированный одним service-owned adapter-ом.
    Url(StartupUrlLocator),
}

/// Подготовленный YtDlp playback source и compact identity выбранной stream-пары.
pub(crate) struct PreparedYtDlpStartupMedia {
    /// Уже открытый demuxer для текущего playback open.
    pub(crate) streaming_media: service_ytdlp::YtDlpStreamingMedia,

    /// Минимальная identity, по которой повторное открытие может восстановить тот же выбор.
    pub(crate) selected_stream_identity: service_ytdlp::YtDlpSelectedStreamIdentity,
}

/// Результат фоновой подготовки CLI YtDlp URL.
type YtDlpStartupResult = std::result::Result<PreparedYtDlpStartupMedia, String>;

/// Результат фоновой подготовки generic direct media URL.
type DirectMediaStartupResult =
    std::result::Result<service_direct_media::DirectMediaOpenResult, String>;

/// Фоновый job, который не блокирует создание окна и UI.
struct YtDlpStartupJob {
    /// URL страницы/ролика, который был передан через CLI.
    source_locator: service_ytdlp::YtDlpMediaLocator,

    /// Текст pending-состояния для центрального overlay.
    pending_message: String,

    /// Exactly-once completion mailbox background resolver-а.
    result_receiver: OwnerMailboxReceiver<(), YtDlpStartupResult>,

    /// JoinHandle нужен для cleanup после получения результата.
    join_handle: Option<JoinHandle<()>>,

    /// Mailbox result удерживается до exact worker exit и успешного join.
    pending_result: Option<YtDlpStartupResult>,

    /// Cooperative cancellation не даёт позднему результату apply authority.
    cancellation_requested: Arc<AtomicBool>,
}

impl YtDlpStartupJob {
    /// Запускает подготовку YtDlp media на отдельном thread-е.
    fn spawn(
        source_locator: service_ytdlp::YtDlpMediaLocator,
        app_config: AppConfig,
        system_capabilities: SystemCapabilities,
        wake_port: AppWakePort,
    ) -> std::result::Result<Self, String> {
        let (result_publisher, result_receiver) = owner_mailbox(wake_port);
        let thread_locator = source_locator.clone();
        let network_config = app_config.network.clone();
        let yt_dlp_config = app_config.yt_dlp.clone();
        let demux_config = app_config.player.demux;
        let preferred_video_codec_order = app_config.player.preferred_video_codec_order;
        let cancellation_requested = Arc::new(AtomicBool::new(false));
        let worker_cancellation_requested = Arc::clone(&cancellation_requested);
        let join_handle = thread::Builder::new()
            .name("yt_dlp-startup-resolver".to_string())
            .spawn(move || {
                let resolve_result = if worker_cancellation_requested.load(Ordering::Acquire) {
                    Err("YtDlp startup preparation отменена shutdown lifecycle".to_string())
                } else {
                    resolve_yt_dlp_startup_media(
                        &thread_locator,
                        &network_config,
                        &yt_dlp_config,
                        &demux_config,
                        &preferred_video_codec_order,
                        &system_capabilities,
                    )
                    .map_err(|error| format!("{error:#}"))
                };

                if worker_cancellation_requested.load(Ordering::Acquire) {
                    return;
                }

                match result_publisher.publish_completion(resolve_result) {
                    Ok(WakeDelivery::EventLoopClosed) => {
                        debug!("Event loop закрыт; YtDlp terminal оставлен без wake retry");
                    }
                    Ok(WakeDelivery::Armed | WakeDelivery::Coalesced) => {}
                    Err(CompletionPublishError::AlreadyPublished) => {
                        warn!(
                            "YtDlp startup resolver попытался опубликовать второй terminal result"
                        );
                    }
                }
            })
            .map_err(|error| format!("Не удалось запустить YtDlp startup resolver: {error}"))?;

        Ok(Self {
            source_locator,
            pending_message: "Подготовка YtDlp stream...".to_string(),
            result_receiver,
            join_handle: Some(join_handle),
            pending_result: None,
            cancellation_requested,
        })
    }

    /// Возвращает pending-текст без доступа к внутреннему channel state.
    fn pending_message(&self) -> &str {
        &self.pending_message
    }

    /// Неблокирующе забирает результат resolver-а, если он уже готов.
    fn try_take_result(&mut self) -> Option<YtDlpStartupResult> {
        let drain = self.result_receiver.drain();
        if drain.completion.is_some() {
            self.pending_result = drain.completion;
        }

        match join_finished_thread(&mut self.join_handle) {
            FinishedThreadJoin::Joined | FinishedThreadJoin::AlreadyJoined => {
                self.pending_result.take().or_else(|| {
                    drain.producer_disconnected_without_completion.then(|| {
                        Err("YtDlp startup resolver завершился без результата".to_string())
                    })
                })
            }
            FinishedThreadJoin::Panicked => {
                self.pending_result = None;
                Some(Err("YtDlp startup resolver завершился panic".to_string()))
            }
            FinishedThreadJoin::StillRunning => None,
        }
    }
}

/// Фоновый job для generic direct media URL.
struct DirectMediaStartupJob {
    /// Direct media URL, который был передан через CLI.
    source_locator: service_direct_media::DirectMediaUrl,

    /// Текст pending-состояния для центрального overlay.
    pending_message: String,

    /// Exactly-once completion mailbox background opener-а.
    result_receiver: OwnerMailboxReceiver<(), DirectMediaStartupResult>,

    /// JoinHandle нужен для cleanup после получения результата.
    join_handle: Option<JoinHandle<()>>,

    /// Mailbox result удерживается до exact worker exit и успешного join.
    pending_result: Option<DirectMediaStartupResult>,

    /// Cooperative cancellation запрещает позднюю публикацию после shutdown.
    cancellation_requested: Arc<AtomicBool>,
}

impl DirectMediaStartupJob {
    /// Запускает подготовку direct media на отдельном thread-е.
    fn spawn(
        source_locator: service_direct_media::DirectMediaUrl,
        app_config: AppConfig,
        wake_port: AppWakePort,
    ) -> std::result::Result<Self, String> {
        let (result_publisher, result_receiver) = owner_mailbox(wake_port);
        let thread_locator = source_locator.clone();
        let network_config = app_config.network.clone();
        let demux_config = app_config.player.demux;
        let cancellation_requested = Arc::new(AtomicBool::new(false));
        let worker_cancellation_requested = Arc::clone(&cancellation_requested);
        let join_handle = thread::Builder::new()
            .name("direct-media-startup-opener".to_string())
            .spawn(move || {
                let open_result = if worker_cancellation_requested.load(Ordering::Acquire) {
                    Err("Direct media startup preparation отменена shutdown lifecycle".to_string())
                } else {
                    resolve_direct_media_startup_media(
                        &thread_locator,
                        &network_config,
                        &demux_config,
                    )
                    .map_err(|error| format!("{error:#}"))
                };

                if worker_cancellation_requested.load(Ordering::Acquire) {
                    return;
                }

                match result_publisher.publish_completion(open_result) {
                    Ok(WakeDelivery::EventLoopClosed) => {
                        debug!("Event loop закрыт; direct-media terminal оставлен без wake retry");
                    }
                    Ok(WakeDelivery::Armed | WakeDelivery::Coalesced) => {}
                    Err(CompletionPublishError::AlreadyPublished) => {
                        warn!(
                            "Direct media startup opener попытался опубликовать второй terminal result"
                        );
                    }
                }
            })
            .map_err(|error| {
                format!("Не удалось запустить direct media startup opener: {error}")
            })?;

        Ok(Self {
            source_locator,
            pending_message: "Подготовка direct media URL...".to_string(),
            result_receiver,
            join_handle: Some(join_handle),
            pending_result: None,
            cancellation_requested,
        })
    }

    /// Возвращает pending-текст без доступа к внутреннему channel state.
    fn pending_message(&self) -> &str {
        &self.pending_message
    }

    /// Неблокирующе забирает результат opener-а, если он уже готов.
    fn try_take_result(&mut self) -> Option<DirectMediaStartupResult> {
        let drain = self.result_receiver.drain();
        if drain.completion.is_some() {
            self.pending_result = drain.completion;
        }

        match join_finished_thread(&mut self.join_handle) {
            FinishedThreadJoin::Joined | FinishedThreadJoin::AlreadyJoined => {
                self.pending_result.take().or_else(|| {
                    drain.producer_disconnected_without_completion.then(|| {
                        Err("Direct media startup opener завершился без результата".to_string())
                    })
                })
            }
            FinishedThreadJoin::Panicked => {
                self.pending_result = None;
                Some(Err(
                    "Direct media startup opener завершился panic".to_string()
                ))
            }
            FinishedThreadJoin::StillRunning => None,
        }
    }
}

/// Владеет shell-состоянием стартового media без знания о renderer/GPU.
pub(crate) struct StartupMediaController {
    /// Process-lifetime wake port общий для mutually-exclusive startup jobs.
    wake_port: AppWakePort,

    /// Media, переданное через CLI или восстановленное после suspend.
    initial_media: Option<InitialMedia>,

    /// Фоновая подготовка CLI YtDlp URL, если она уже запущена.
    yt_dlp_startup_job: Option<YtDlpStartupJob>,

    /// Фоновая подготовка generic direct media URL, если она уже запущена.
    direct_media_startup_job: Option<DirectMediaStartupJob>,

    /// Local CLI/restore preparation принадлежит startup owner-у, а не UI picker-у.
    local_startup_job: Option<crate::local_file_open::LocalFileOpenJob>,

    /// Winner/fallback state удерживает prepared ownership до allocator gate.
    orchestration: StartupMediaOrchestration,

    /// URL replacement draft не получает Item ID до ReadyToCommit reservation.
    cli_url_target_draft: Option<playlist_core::PlaylistItemDraft>,

    /// Reusable preparation inputs нужны позднему restored fallback после CLI failure.
    startup_config: Option<AppConfig>,
    system_capabilities: Option<SystemCapabilities>,

    /// Startup-ошибка shell-слоя, которую нужно показать после создания UI.
    startup_error: Option<String>,

    /// Terminal shutdown закрывает admission новых startup jobs.
    terminal_shutdown_started: bool,

    /// Завершённый controller отличает первый terminal call от повторного.
    terminal_shutdown_completed: bool,
}

impl StartupMediaController {
    /// Создаёт controller из startup-состояния, которое было собрано до запуска окна.
    #[cfg(test)]
    pub(crate) fn new(initial_media: Option<InitialMedia>, startup_error: Option<String>) -> Self {
        Self::with_wake_port(
            initial_media,
            startup_error,
            AppWakePort::disconnected(AppWakeOwner::StartupMedia),
        )
    }

    /// Production constructor получает process-lifetime wake port от `AppShell`.
    pub(crate) fn with_wake_port(
        initial_media: Option<InitialMedia>,
        startup_error: Option<String>,
        wake_port: AppWakePort,
    ) -> Self {
        let cli_requested = initial_media.is_some();
        Self {
            wake_port,
            initial_media,
            yt_dlp_startup_job: None,
            direct_media_startup_job: None,
            local_startup_job: None,
            orchestration: StartupMediaOrchestration::new(cli_requested),
            cli_url_target_draft: None,
            startup_config: None,
            system_capabilities: None,
            startup_error,
            terminal_shutdown_started: false,
            terminal_shutdown_completed: false,
        }
    }

    /// Возвращает копию startup-ошибки для инициализации `AppState`.
    pub(crate) fn startup_error_message(&self) -> Option<String> {
        self.startup_error.clone()
    }

    /// Возвращает pending-текст активного startup job.
    pub(crate) fn pending_message(&self) -> Option<&str> {
        self.yt_dlp_startup_job
            .as_ref()
            .map(YtDlpStartupJob::pending_message)
            .or_else(|| {
                self.direct_media_startup_job
                    .as_ref()
                    .map(DirectMediaStartupJob::pending_message)
            })
            .or_else(|| {
                self.local_startup_job
                    .as_ref()
                    .map(|_| "Подготовка local media...")
            })
    }

    /// Сообщает shell scheduler-у, нужно ли продолжать polling startup jobs.
    pub(crate) fn has_pending_startup_job(&self) -> bool {
        self.yt_dlp_startup_job.is_some()
            || self.direct_media_startup_job.is_some()
            || self.local_startup_job.is_some()
            || self.orchestration.has_pending_work()
    }

    /// Read-only phase не даёт shell-у доступа к prepared media ownership.
    #[allow(dead_code, reason = "Session 18 renders the startup phase read model")]
    pub(crate) const fn phase(&self) -> StartupMediaPhase {
        self.orchestration.phase
    }

    /// Informational D15 warning не является gate и переживает renderer suspend.
    #[allow(dead_code, reason = "Session 18 renders the process-global warning")]
    pub(crate) const fn has_sensitive_cli_persistence_warning(&self) -> bool {
        self.orchestration.sensitive_cli_persistence_warning
    }

    /// Запускает отложенное стартовое media после того, как `AppState` уже создан.
    pub(crate) fn start_pending_initial_media(
        &mut self,
        app_state: &mut AppState,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
        app_config: &AppConfig,
        system_capabilities: &SystemCapabilities,
    ) {
        self.startup_config = Some(app_config.clone());
        self.system_capabilities = Some(system_capabilities.clone());
        self.orchestration.expected_restore_generation =
            Some(playlist_runtime.playlist_startup_view().restore_generation);
        if let Some(pending_message) = self.pending_message() {
            app_state.set_startup_pending(pending_message.to_string());
        }

        let Some(initial_media) = self.initial_media.take() else {
            self.drive_startup_orchestration(app_state, playlist_runtime, renderer);
            return;
        };

        self.orchestration
            .begin_target(StartupMediaTarget::CliReplacement);

        let trusted_intent = match initial_media {
            InitialMedia::File(path) => {
                crate::playlist_runtime::TrustedStartupQueueReplacementIntent::local_file(path)
            }
            InitialMedia::Url(locator) => {
                self.orchestration.sensitive_cli_persistence_warning =
                    locator.requires_sensitive_persistence_acknowledgement();
                let domain_locator = match locator.to_playlist_locator() {
                    Ok(locator) => locator,
                    Err(error) => {
                        self.orchestration.preparation_failed();
                        app_state.set_startup_error(format!(
                            "Не удалось сохранить reopenable CLI URL identity: {error}"
                        ));
                        return;
                    }
                };
                self.cli_url_target_draft = Some(playlist_core::PlaylistItemDraft::url(
                    domain_locator,
                    playlist_core::CachedPlaylistMetadata::new(
                        locator.safe_label(),
                        playlist_core::PlaylistMediaKind::Unknown,
                    ),
                ));
                crate::playlist_runtime::TrustedStartupQueueReplacementIntent::service_url(locator)
            }
        };
        let admitted =
            match playlist_runtime.admit_trusted_startup_queue_replacement(trusted_intent) {
                Ok(admitted) => admitted,
                Err(error) => {
                    self.orchestration.preparation_failed();
                    warn!(error = %error, "Trusted startup media admission отклонён");
                    app_state.set_startup_error(format!(
                        "Не удалось начать стартовое открытие media: {error}"
                    ));
                    return;
                }
            };

        match admitted {
            crate::playlist_runtime::AdmittedQueueReplacementIntent::LocalFile(local_open) => {
                let safe_label = crate::playlist_runtime::safe_local_open_label(
                    local_open.path_for_safe_label(),
                );
                info!(source = %safe_label, "Автозагрузка файла из CLI");
                match crate::local_file_open::LocalFileOpenJob::spawn_preparation(
                    local_open.into_path(),
                    app_config.player.demux,
                    self.wake_port.clone(),
                ) {
                    Ok(job) => self.local_startup_job = Some(job),
                    Err(error) => {
                        self.orchestration.preparation_failed();
                        app_state.set_startup_error(error);
                    }
                }
            }
            crate::playlist_runtime::AdmittedQueueReplacementIntent::ServiceUrl(url_open) => {
                let locator = url_open.into_locator();
                info!(source = %locator.safe_label(), "Автозагрузка service URL из CLI");
                locator.start(self, app_state, app_config, system_capabilities);
            }
        }
    }

    /// Неблокирующе опрашивает результаты фоновой подготовки startup URL-ов.
    pub(crate) fn poll_startup_jobs(
        &mut self,
        app_state: &mut AppState,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) -> bool {
        self.drive_startup_orchestration(app_state, playlist_runtime, renderer)
    }

    /// Запускает background resolve для CLI YtDlp URL и сразу обновляет UI state.
    pub(crate) fn start_yt_dlp_startup_job(
        &mut self,
        source_locator: service_ytdlp::YtDlpMediaLocator,
        app_state: &mut AppState,
        app_config: &AppConfig,
        system_capabilities: &SystemCapabilities,
    ) {
        if let Some(error) = self.startup_job_admission_error() {
            self.orchestration.preparation_failed();
            self.startup_error = Some(error.clone());
            app_state.set_startup_error(error);
            return;
        }
        app_state.set_startup_pending("Подготовка YtDlp stream...".to_string());
        match YtDlpStartupJob::spawn(
            source_locator,
            app_config.clone(),
            system_capabilities.clone(),
            self.wake_port.clone(),
        ) {
            Ok(job) => {
                self.startup_error = None;
                self.yt_dlp_startup_job = Some(job);
            }
            Err(error) => {
                self.orchestration.preparation_failed();
                warn!(error = %error, "Не удалось запустить YtDlp startup resolver");
                let startup_error = format!("NetworkError: YtDlp error: {error}");
                self.startup_error = Some(startup_error.clone());
                app_state.set_startup_error(startup_error);
            }
        }
    }

    /// Запускает background open для CLI direct media URL и сразу обновляет UI state.
    pub(crate) fn start_direct_media_startup_job(
        &mut self,
        source_locator: service_direct_media::DirectMediaUrl,
        app_state: &mut AppState,
        app_config: &AppConfig,
    ) {
        if let Some(error) = self.startup_job_admission_error() {
            self.orchestration.preparation_failed();
            self.startup_error = Some(error.clone());
            app_state.set_startup_error(error);
            return;
        }
        app_state.set_startup_pending("Подготовка direct media URL...".to_string());
        match DirectMediaStartupJob::spawn(
            source_locator,
            app_config.clone(),
            self.wake_port.clone(),
        ) {
            Ok(job) => {
                self.startup_error = None;
                self.direct_media_startup_job = Some(job);
            }
            Err(error) => {
                self.orchestration.preparation_failed();
                warn!(error = %error, "Не удалось запустить direct media startup opener");
                let startup_error = format!("NetworkError: Direct media error: {error}");
                self.startup_error = Some(startup_error.clone());
                app_state.set_startup_error(startup_error);
            }
        }
    }
}

/// Выполняет production chain: service candidates -> capability selection -> demux open.
pub(crate) fn resolve_yt_dlp_startup_media(
    source_locator: &service_ytdlp::YtDlpMediaLocator,
    network_config: &NetworkConfig,
    yt_dlp_config: &YtDlpConfig,
    demux_config: &PlayerDemuxConfig,
    preferred_video_codec_order: &[ConfigVideoCodec],
    system_capabilities: &SystemCapabilities,
) -> Result<PreparedYtDlpStartupMedia> {
    let stream_candidates =
        service_ytdlp::resolve_yt_dlp_stream_candidates_with_config(source_locator, yt_dlp_config)
            .context("Не удалось получить YtDlp stream candidates")?;
    let selected_stream = select_yt_dlp_startup_candidate_with_codec_order(
        &stream_candidates,
        preferred_video_codec_order,
        yt_dlp_config.hdr_selection,
        system_capabilities,
    )
    .context("Не удалось выбрать YtDlp stream по codec policy и system capabilities")?;

    let selected_stream_identity = service_ytdlp::YtDlpSelectedStreamIdentity::from_candidates(
        &stream_candidates,
        &selected_stream.stream_id,
    )
    .context("Не удалось сохранить выбранную YtDlp stream identity")?;
    let streaming_media = service_ytdlp::open_streaming_media_from_candidates_with_demux_config(
        source_locator,
        &stream_candidates,
        &selected_stream.stream_id,
        network_config,
        yt_dlp_config,
        demux_config,
    )
    .with_context(|| {
        format!(
            "Не удалось открыть выбранный YtDlp stream {}",
            selected_stream.stream_id
        )
    })?;

    Ok(PreparedYtDlpStartupMedia {
        streaming_media,
        selected_stream_identity,
    })
}

/// Выполняет generic direct media open chain без YtDlp-specific semantics.
pub(crate) fn resolve_direct_media_startup_media(
    source_locator: &service_direct_media::DirectMediaUrl,
    network_config: &NetworkConfig,
    demux_config: &PlayerDemuxConfig,
) -> Result<service_direct_media::DirectMediaOpenResult> {
    service_direct_media::open_direct_media_url(source_locator, network_config, demux_config)
        .context("Не удалось открыть direct media URL")
}

/// Делегирует service-owned policy выбор до открытия media bytes.
fn select_yt_dlp_startup_candidate_with_codec_order(
    stream_candidates: &YtDlpStreamCandidates,
    preferred_video_codec_order: &[ConfigVideoCodec],
    hdr_selection: YtDlpHdrSelection,
    system_capabilities: &SystemCapabilities,
) -> std::result::Result<SelectedVideoStream, service_ytdlp::YtDlpStreamSelectionError> {
    let preferred_runtime_codecs = preferred_video_codec_order
        .iter()
        .copied()
        .map(runtime_video_codec)
        .collect::<Vec<_>>();

    service_ytdlp::select_yt_dlp_stream(
        stream_candidates,
        &preferred_runtime_codecs,
        hdr_selection,
        system_capabilities,
    )
}

/// Test helper с default codec policy и явной HDR policy.
#[cfg(test)]
fn select_yt_dlp_startup_candidate(
    stream_candidates: &YtDlpStreamCandidates,
    hdr_selection: YtDlpHdrSelection,
    system_capabilities: &SystemCapabilities,
) -> std::result::Result<SelectedVideoStream, service_ytdlp::YtDlpStreamSelectionError> {
    select_yt_dlp_startup_candidate_with_codec_order(
        stream_candidates,
        &AppConfig::default().player.preferred_video_codec_order,
        hdr_selection,
        system_capabilities,
    )
}

/// Сопоставляет user-facing codec policy с нейтральным capability vocabulary.
pub(crate) const fn runtime_video_codec(codec: ConfigVideoCodec) -> RuntimeVideoCodec {
    match codec {
        ConfigVideoCodec::Vp9 => RuntimeVideoCodec::Vp9,
        ConfigVideoCodec::Av1 => RuntimeVideoCodec::Av1,
        ConfigVideoCodec::H264 => RuntimeVideoCodec::H264,
        ConfigVideoCodec::H265 => RuntimeVideoCodec::H265,
        ConfigVideoCodec::Vp8 => RuntimeVideoCodec::Vp8,
    }
}

/// Классифицирует уже разобранный typed media intent после получения process lease.
///
/// Только валидный UTF-8 может быть URL. Native non-UTF-8 значение без lossy
/// преобразования остаётся локальным `PathBuf`.
pub(crate) fn resolve_initial_media_argument(
    argument: Option<std::ffi::OsString>,
    app_config: &AppConfig,
) -> (Option<InitialMedia>, Option<String>) {
    let Some(argument) = argument else {
        return (None, None);
    };
    let utf8_argument = match argument.into_string() {
        Ok(argument) => argument,
        Err(native_argument) => {
            return (
                Some(InitialMedia::File(PathBuf::from(native_argument))),
                None,
            );
        }
    };

    match classify_startup_url(&utf8_argument) {
        StartupUrlClassification::NotUrl => {}
        StartupUrlClassification::Supported(locator) => {
            info!(source = %locator.safe_label(), "CLI аргумент принят URL service adapter-ом");
            if let Err(safe_error) = locator.validate_config(app_config) {
                return (None, Some(safe_error));
            }
            return (Some(InitialMedia::Url(locator)), None);
        }
        StartupUrlClassification::Unsupported { safe_error } => {
            return (None, Some(safe_error));
        }
    }

    // Всё остальное считаем локальным путём, как работало раньше.
    (Some(InitialMedia::File(PathBuf::from(utf8_argument))), None)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;
    use std::time::Duration;

    use capability_core::{
        BackendCapabilities, BackendDriverInfo, BackendProbeStatus, SupportedVideoOutput,
        SystemCapabilities,
    };
    use codec_core::{
        BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, DecodeBackendId,
        MatrixCoefficients, SupportedVideoDecodeFormat, TransferFunction, VideoCodec,
        VideoColorMetadata, VideoDecodeRequirement, VideoProfile, Vp9Profile,
    };
    use render_core::RenderCapabilities;
    use service_ytdlp::{
        YtDlpDirectStreamDescriptor, YtDlpDynamicRange, YtDlpInsufficientVideoMetadata,
        YtDlpStreamCandidate, YtDlpStreamCandidates, YtDlpStreamKind, YtDlpVideoRequirement,
    };
    use source_core::SourceValidators;
    use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

    use super::*;
    use crate::process_shutdown::{ProcessOwnerShutdownOutcome, ShutdownDeadline};

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

    fn yt_dlp_descriptor(kind: YtDlpStreamKind, format_id: &str) -> YtDlpDirectStreamDescriptor {
        let kind_label = match kind {
            YtDlpStreamKind::Video => "video",
            YtDlpStreamKind::Audio => "audio",
        };

        YtDlpDirectStreamDescriptor {
            kind,
            url: service_ytdlp::YtDlpDirectStreamUrl::from_secret_for_open(format!(
                "http://media.test/{format_id}.webm"
            )),
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
        let color = match profile {
            Vp9Profile::Profile2 | Vp9Profile::Profile3 => VideoColorMetadata::container(
                ColorRange::Limited,
                MatrixCoefficients::Bt2020,
                ColorPrimaries::Bt2020,
                TransferFunction::Pq,
                None,
            ),
            Vp9Profile::Profile0 | Vp9Profile::Profile1 => VideoColorMetadata::sdr_bt709_limited(),
        };

        VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_profile(VideoProfile::Vp9(profile))
            .with_bit_depth(bit_depth)
            .with_chroma(ChromaSubsampling::Yuv420)
            .with_resolution(1920, 1080)
            .with_frame_rate(60.0)
            .with_color(color)
    }

    fn ready_yt_dlp_candidate(
        stream_id: &str,
        format_id: &str,
        requirement: VideoDecodeRequirement,
        quality_score: i64,
    ) -> YtDlpStreamCandidate {
        let dynamic_range = if requirement.requires_hdr_processing() {
            YtDlpDynamicRange::Hdr
        } else {
            YtDlpDynamicRange::Sdr
        };
        YtDlpStreamCandidate {
            stream_id: stream_id.to_string(),
            format_id: Some(format!("{format_id}+251")),
            video: yt_dlp_descriptor(YtDlpStreamKind::Video, format_id),
            audio: Some(yt_dlp_descriptor(YtDlpStreamKind::Audio, "251")),
            height: requirement.height,
            fps: requirement.fps,
            vcodec: Some("vp09.00.10.08".to_string()),
            acodec: Some("opus".to_string()),
            dynamic_range,
            video_requirement: YtDlpVideoRequirement::Ready(requirement),
            quality_score,
        }
    }

    fn yt_dlp_candidates(candidates: Vec<YtDlpStreamCandidate>) -> YtDlpStreamCandidates {
        YtDlpStreamCandidates {
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
    fn pending_message_reports_existing_yt_dlp_job() {
        let wake_port = AppWakePort::disconnected(AppWakeOwner::StartupMedia);
        let (_result_publisher, result_receiver) = owner_mailbox(wake_port.clone());
        let controller = StartupMediaController {
            wake_port,
            initial_media: None,
            yt_dlp_startup_job: Some(YtDlpStartupJob {
                source_locator: service_ytdlp::parse_yt_dlp_media_locator(
                    "https://www.youtube.com/watch?v=test",
                )
                .expect("test locator должен проходить service parse"),
                pending_message: "Подготовка YtDlp stream...".to_string(),
                result_receiver,
                join_handle: None,
                pending_result: None,
                cancellation_requested: Arc::new(AtomicBool::new(false)),
            }),
            direct_media_startup_job: None,
            local_startup_job: None,
            orchestration: StartupMediaOrchestration::new(false),
            cli_url_target_draft: None,
            startup_config: None,
            system_capabilities: None,
            startup_error: None,
            terminal_shutdown_started: false,
            terminal_shutdown_completed: false,
        };

        assert!(controller.has_pending_startup_job());
        assert_eq!(
            controller.pending_message(),
            Some("Подготовка YtDlp stream...")
        );
        assert!(
            controller.startup_job_admission_error().is_some(),
            "single-startup-job boundary должен отвергать replacement"
        );
    }

    /// Собирает controller с synthetic worker-ом без network/media locator leakage.
    fn controller_with_test_yt_dlp_thread(join_handle: JoinHandle<()>) -> StartupMediaController {
        let wake_port = AppWakePort::disconnected(AppWakeOwner::StartupMedia);
        let (_result_publisher, result_receiver) = owner_mailbox(wake_port.clone());
        StartupMediaController {
            wake_port,
            initial_media: None,
            yt_dlp_startup_job: Some(YtDlpStartupJob {
                source_locator: service_ytdlp::parse_yt_dlp_media_locator(
                    "https://www.youtube.com/watch?v=test",
                )
                .expect("test locator должен проходить service parse"),
                pending_message: "Подготовка YtDlp stream...".to_string(),
                result_receiver,
                join_handle: Some(join_handle),
                pending_result: None,
                cancellation_requested: Arc::new(AtomicBool::new(false)),
            }),
            direct_media_startup_job: None,
            local_startup_job: None,
            orchestration: StartupMediaOrchestration::new(false),
            cli_url_target_draft: None,
            startup_config: None,
            system_capabilities: None,
            startup_error: None,
            terminal_shutdown_started: false,
            terminal_shutdown_completed: false,
        }
    }

    #[test]
    fn startup_shutdown_timeout_retains_handle_and_later_reaps_it() {
        let release = Arc::new(AtomicBool::new(false));
        let worker_release = Arc::clone(&release);
        let mut controller = controller_with_test_yt_dlp_thread(std::thread::spawn(move || {
            while !worker_release.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
        }));

        assert_eq!(
            controller.shutdown_until(ShutdownDeadline::after(Duration::from_millis(1))),
            ProcessOwnerShutdownOutcome::TimedOut { pending_threads: 1 }
        );
        assert!(controller.yt_dlp_startup_job.is_some());

        release.store(true, Ordering::Release);
        assert_eq!(
            controller.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
            ProcessOwnerShutdownOutcome::Completed
        );
        assert_eq!(
            controller.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
            ProcessOwnerShutdownOutcome::AlreadyCompleted
        );
    }

    #[test]
    fn startup_shutdown_reports_worker_panic() {
        let mut controller = controller_with_test_yt_dlp_thread(std::thread::spawn(|| {
            panic!("expected startup panic");
        }));

        assert_eq!(
            controller.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
            ProcessOwnerShutdownOutcome::ThreadPanicked {
                panicked_threads: 1,
                pending_threads: 0,
            }
        );
    }

    #[test]
    fn cli_route_keeps_local_path_unchanged() {
        let (initial_media, startup_error) = resolve_initial_media_argument(
            Some(OsString::from("/tmp/sample.mp4")),
            &AppConfig::default(),
        );

        assert!(startup_error.is_none());
        assert!(matches!(
            initial_media,
            Some(InitialMedia::File(path)) if path == Path::new("/tmp/sample.mp4")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn cli_route_keeps_non_utf8_argument_as_native_local_path() {
        use std::os::unix::ffi::OsStringExt;

        let native_path = OsString::from_vec(b"/tmp/movie-\xFF.mkv".to_vec());
        let expected_path = std::path::PathBuf::from(native_path.clone());
        let (initial_media, startup_error) =
            resolve_initial_media_argument(Some(native_path), &AppConfig::default());

        assert!(startup_error.is_none());
        assert!(matches!(
            initial_media,
            Some(InitialMedia::File(path)) if path == expected_path
        ));
    }

    #[test]
    fn cli_route_sends_yt_dlp_host_to_yt_dlp_path() {
        let (initial_media, startup_error) = resolve_initial_media_argument(
            Some(OsString::from("https://youtu.be/video-id")),
            &AppConfig::default(),
        );

        assert!(startup_error.is_none());
        assert!(matches!(
            initial_media,
            Some(InitialMedia::Url(locator))
                if locator.to_playlist_locator().is_ok_and(|domain_locator| {
                    domain_locator.expose_secret_for_persistence()
                        == "https://youtu.be/video-id"
                })
        ));
    }

    #[test]
    fn cli_route_sends_supported_http_media_to_direct_path() {
        let (initial_media, startup_error) = resolve_initial_media_argument(
            Some(OsString::from("https://cdn.example.test/video.mp4?token=1")),
            &AppConfig::default(),
        );

        assert!(startup_error.is_none());
        assert!(matches!(
            initial_media,
            Some(InitialMedia::Url(locator))
                if locator.to_playlist_locator().is_ok_and(|domain_locator| {
                    domain_locator.expose_secret_for_persistence()
                        == "https://cdn.example.test/video.mp4?token=1"
                })
        ));
    }

    #[test]
    fn cli_route_sends_quicktime_mov_http_media_to_direct_path() {
        let (initial_media, startup_error) = resolve_initial_media_argument(
            Some(OsString::from(
                "https://cdn.example.test/camera/ios-hevc-main10-aac-4k60.MOV",
            )),
            &AppConfig::default(),
        );

        assert!(startup_error.is_none());
        assert!(matches!(
            initial_media,
            Some(InitialMedia::Url(locator))
                if locator.to_playlist_locator().is_ok_and(|domain_locator| {
                    domain_locator.expose_secret_for_persistence()
                        == "https://cdn.example.test/camera/ios-hevc-main10-aac-4k60.MOV"
                })
        ));
    }

    #[test]
    fn cli_route_sends_http_page_without_direct_extension_to_yt_dlp_fallback() {
        let (initial_media, startup_error) = resolve_initial_media_argument(
            Some(OsString::from("https://192.0.2.10/media")),
            &AppConfig::default(),
        );

        assert!(startup_error.is_none());
        assert!(matches!(initial_media, Some(InitialMedia::Url(_))));
    }

    #[test]
    fn cli_route_rejects_unsupported_media_protocol() {
        let (initial_media, startup_error) = resolve_initial_media_argument(
            Some(OsString::from("rtsp://192.0.2.10/video.mp4")),
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
    fn yt_dlp_startup_selection_picks_supported_candidate_before_demux_open() {
        let candidates = yt_dlp_candidates(vec![
            ready_yt_dlp_candidate(
                "hdr-p010",
                "315",
                vp9_requirement(Vp9Profile::Profile2, BitDepth::Ten),
                10_000,
            ),
            ready_yt_dlp_candidate(
                "sdr-nv12",
                "303",
                vp9_requirement(Vp9Profile::Profile0, BitDepth::Eight),
                1_000,
            ),
        ]);

        let selected = select_yt_dlp_startup_candidate(
            &candidates,
            YtDlpHdrSelection::SdrOnly,
            &capabilities_with_vp9_profile0(),
        )
        .expect("supported SDR candidate is selected");

        assert_eq!(selected.stream_id, "sdr-nv12");
    }

    #[test]
    fn yt_dlp_startup_codec_policy_uses_first_supported_configured_codec() {
        let candidates = yt_dlp_candidates(vec![ready_yt_dlp_candidate(
            "vp9-video",
            "247",
            vp9_requirement(Vp9Profile::Profile0, BitDepth::Eight),
            100,
        )]);
        let capabilities = capabilities_with_vp9_profile0();

        let selected = select_yt_dlp_startup_candidate_with_codec_order(
            &candidates,
            &[ConfigVideoCodec::Av1, ConfigVideoCodec::Vp9],
            YtDlpHdrSelection::SdrOnly,
            &capabilities,
        )
        .expect("policy must continue to the first supported configured codec");
        assert_eq!(selected.stream_id, "vp9-video");

        let error = select_yt_dlp_startup_candidate_with_codec_order(
            &candidates,
            &[ConfigVideoCodec::Av1],
            YtDlpHdrSelection::SdrOnly,
            &capabilities,
        )
        .expect_err("codec omitted from policy must not be selected");
        assert!(matches!(
            error,
            service_ytdlp::YtDlpStreamSelectionError::NoPlayableSdr { .. }
        ));
    }

    #[test]
    fn yt_dlp_startup_selection_returns_typed_capability_rejection() {
        let candidates = yt_dlp_candidates(vec![ready_yt_dlp_candidate(
            "sdr-nv12",
            "303",
            vp9_requirement(Vp9Profile::Profile0, BitDepth::Eight),
            1_000,
        )]);

        let error = select_yt_dlp_startup_candidate(
            &candidates,
            YtDlpHdrSelection::SdrOnly,
            &SystemCapabilities::empty(1),
        )
        .expect_err("empty capabilities reject ready candidate");

        assert!(matches!(
            error,
            service_ytdlp::YtDlpStreamSelectionError::NoPlayableSdr { rejections }
                if rejections.iter().any(|rejection| matches!(
                    rejection.reason,
                    service_ytdlp::YtDlpCandidateRejectionReason::Capability(_)
                ))
        ));
    }

    #[test]
    fn yt_dlp_startup_selection_reports_insufficient_service_metadata() {
        let mut candidate = ready_yt_dlp_candidate(
            "plain-vp9",
            "248",
            VideoDecodeRequirement::new(VideoCodec::Vp9),
            1_000,
        );
        candidate.video_requirement =
            YtDlpVideoRequirement::Insufficient(YtDlpInsufficientVideoMetadata {
                reason: "missing VP9 profile metadata".to_string(),
                partial_requirement: Some(VideoDecodeRequirement::new(VideoCodec::Vp9)),
            });
        let candidates = yt_dlp_candidates(vec![candidate]);

        let error = select_yt_dlp_startup_candidate(
            &candidates,
            YtDlpHdrSelection::SdrOnly,
            &capabilities_with_vp9_profile0(),
        )
        .expect_err("insufficient service metadata is rejected before selection");

        assert!(matches!(
            error,
            service_ytdlp::YtDlpStreamSelectionError::NoPlayableSdr { rejections }
                if rejections.iter().any(|rejection| {
                    rejection.stream_id == "plain-vp9"
                        && matches!(
                            &rejection.reason,
                            service_ytdlp::YtDlpCandidateRejectionReason::InsufficientVideoMetadata { reason }
                                if reason.contains("missing VP9 profile metadata")
                        )
                })
        ));
    }

    #[test]
    fn yt_dlp_startup_selection_rejects_candidate_without_audio_companion() {
        let mut candidate = ready_yt_dlp_candidate(
            "video-without-audio",
            "303",
            vp9_requirement(Vp9Profile::Profile0, BitDepth::Eight),
            1_000,
        );
        candidate.audio = None;
        let candidates = yt_dlp_candidates(vec![candidate]);

        let error = select_yt_dlp_startup_candidate(
            &candidates,
            YtDlpHdrSelection::SdrOnly,
            &capabilities_with_vp9_profile0(),
        )
        .expect_err("video-only candidate is not openable by current service path");

        assert!(matches!(
            error,
            service_ytdlp::YtDlpStreamSelectionError::NoPlayableSdr { rejections }
                if rejections.iter().any(|rejection| {
                    rejection.stream_id == "video-without-audio"
                        && rejection.reason
                            == service_ytdlp::YtDlpCandidateRejectionReason::MissingAudioCompanion
                })
        ));
    }

    #[test]
    fn yt_dlp_startup_sdr_only_selects_sdr_candidate() {
        // Явная пользовательская политика SdrOnly не допускает HDR даже при
        // более высоком quality score.
        let mut hdr_requirement = vp9_requirement(Vp9Profile::Profile2, BitDepth::Ten);
        hdr_requirement.hdr = true;
        let candidates = yt_dlp_candidates(vec![
            ready_yt_dlp_candidate("hdr-stream", "315", hdr_requirement, 10_000),
            ready_yt_dlp_candidate(
                "sdr-nv12",
                "303",
                vp9_requirement(Vp9Profile::Profile0, BitDepth::Eight),
                1_000,
            ),
        ]);

        let selected = select_yt_dlp_startup_candidate(
            &candidates,
            YtDlpHdrSelection::SdrOnly,
            &capabilities_with_vp9_profile0(),
        )
        .expect("SDR candidate остаётся выбираемым при отключённом HDR");

        assert_eq!(selected.stream_id, "sdr-nv12");
    }

    #[test]
    fn yt_dlp_startup_sdr_only_reports_no_playable_sdr_for_hdr_only_list() {
        // Единственный кандидат — HDR, поэтому SdrOnly возвращает отдельную
        // typed ошибку отсутствия playable SDR.
        let mut hdr_requirement = vp9_requirement(Vp9Profile::Profile2, BitDepth::Ten);
        hdr_requirement.hdr = true;
        let candidates = yt_dlp_candidates(vec![ready_yt_dlp_candidate(
            "hdr-only",
            "315",
            hdr_requirement,
            10_000,
        )]);

        let error = select_yt_dlp_startup_candidate(
            &candidates,
            YtDlpHdrSelection::SdrOnly,
            &capabilities_with_vp9_profile0(),
        )
        .expect_err("HDR-only кандидат отфильтрован до capability selection");

        assert!(matches!(
            error,
            service_ytdlp::YtDlpStreamSelectionError::NoPlayableSdr { .. }
        ));
    }
}
