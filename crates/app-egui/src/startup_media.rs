//! Process-lifetime startup media jobs и typed CLI routing.
//!
//! Root хранит job envelopes, а child modules владеют orchestration и shutdown.
//! Подготовленный source остаётся uncommitted до общего strong-install barrier-а.
//! Cancellation проходит в resolver и transport одной startup generation.

use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use capability_core::SystemCapabilities;
use codec_core::VideoCodec as RuntimeVideoCodec;
use fastiplayer_config::{
    AppConfig, NetworkConfig, PlayerDemuxConfig, VideoCodec as ConfigVideoCodec,
};
use tracing::{debug, info, warn};

#[cfg(test)]
use crate::app_wake::AppWakeOwner;
use crate::app_wake::{
    AppWakePort, CompletionPublishError, OwnerMailboxReceiver, WakeDelivery, owner_mailbox,
};
use crate::process_shutdown::{FinishedThreadJoin, join_finished_thread};
use crate::startup_readiness::{
    StartupAudioExpectation, StartupMediaOpenKind, StartupPlaybackExpectation,
    StartupReadinessExpectation, StartupTargetExpectation,
};
use crate::state::AppState;
use crate::url_service_adapter::{
    StartupUrlClassification, StartupUrlLocator, classify_startup_url,
};

pub(crate) mod native_dash;
pub(crate) mod native_hds;
pub(crate) mod native_hls;
pub(crate) mod native_smooth;
mod orchestration;
mod pending_install;
mod playlist;
mod shutdown;
mod yt_dlp;

use native_dash::NativeDashStartupJob;
use native_hds::NativeHdsStartupJob;
use native_hls::NativeHlsStartupJob;
use native_smooth::NativeSmoothStartupJob;
pub(crate) use orchestration::StartupMediaPhase;
#[cfg(test)]
pub(crate) use orchestration::apply_restored_playback_policy;
use orchestration::{StartupMediaOrchestration, StartupMediaTarget};
pub(crate) use yt_dlp::PreparedYtDlpStartupMedia;

/// Интервал polling-а фоновой подготовки startup media, когда playback ещё не активен.
pub(crate) const STARTUP_MEDIA_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Media, которое нужно автоматически открыть после создания окна.
#[derive(Debug)]
pub(crate) enum InitialMedia {
    /// Локальный файл.
    File(PathBuf),

    /// Recognized local playlist, который проходит trusted `StartupReplace`.
    Playlist(PathBuf),

    /// URL, уже классифицированный одним service-owned adapter-ом.
    Url(StartupUrlLocator),
}

/// Результат фоновой подготовки CLI YtDlp URL.
type YtDlpStartupResult = std::result::Result<PreparedYtDlpStartupMedia, String>;

/// Результат фоновой подготовки generic direct media URL.
type DirectMediaStartupResult =
    std::result::Result<crate::direct_progressive_open::DirectProgressiveOpenResult, String>;

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

    /// Тот же cancellation state прерывает S22 transport/demux blocking work.
    source_cancellation: source_core::CancellationToken,
}

impl YtDlpStartupJob {
    /// Запускает подготовку YtDlp media на отдельном thread-е.
    fn spawn(
        source_locator: service_ytdlp::YtDlpMediaLocator,
        app_config: AppConfig,
        system_capabilities: SystemCapabilities,
        audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
        wake_port: AppWakePort,
    ) -> std::result::Result<Self, String> {
        let (result_publisher, result_receiver) = owner_mailbox(wake_port);
        let thread_locator = source_locator.clone();
        let cancellation_requested = Arc::new(AtomicBool::new(false));
        let worker_cancellation_requested = Arc::clone(&cancellation_requested);
        let source_cancellation = source_core::CancellationToken::new();
        let worker_source_cancellation = source_cancellation.clone();
        let join_handle = thread::Builder::new()
            .name("yt_dlp-startup-resolver".to_string())
            .spawn(move || {
                let resolve_result = if worker_cancellation_requested.load(Ordering::Acquire) {
                    Err("YtDlp startup preparation отменена shutdown lifecycle".to_string())
                } else {
                    resolve_yt_dlp_startup_media(
                        &thread_locator,
                        &app_config,
                        &system_capabilities,
                        audio_capabilities,
                        web_media_core::ExtractorInvocationReason::PageMediaResolution,
                        worker_source_cancellation,
                        || worker_cancellation_requested.load(Ordering::Acquire),
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
            source_cancellation,
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
                        source_core::CancellationToken::new(),
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

    /// Mutually-exclusive native HLS admission/fallback job.
    native_hls_startup_job: Option<NativeHlsStartupJob>,

    /// Mutually-exclusive native static DASH admission/fallback job.
    native_dash_startup_job: Option<NativeDashStartupJob>,

    /// Mutually-exclusive native HDS admission/fallback job.
    native_hds_startup_job: Option<NativeHdsStartupJob>,

    /// Mutually-exclusive native Smooth admission/fallback job.
    native_smooth_startup_job: Option<NativeSmoothStartupJob>,

    /// Local CLI/restore preparation принадлежит startup owner-у, а не UI picker-у.
    local_startup_job: Option<crate::local_file_open::LocalFileOpenJob>,

    /// Playlist parser/preview/commit живёт в PlaylistRuntime, а controller хранит winner intent.
    startup_playlist_pending: bool,

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
            native_hls_startup_job: None,
            native_dash_startup_job: None,
            native_hds_startup_job: None,
            native_smooth_startup_job: None,
            local_startup_job: None,
            startup_playlist_pending: false,
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
                self.native_hls_startup_job
                    .as_ref()
                    .map(NativeHlsStartupJob::pending_message)
            })
            .or_else(|| {
                self.native_dash_startup_job
                    .as_ref()
                    .map(NativeDashStartupJob::pending_message)
            })
            .or_else(|| {
                self.native_hds_startup_job
                    .as_ref()
                    .map(NativeHdsStartupJob::pending_message)
            })
            .or_else(|| {
                self.native_smooth_startup_job
                    .as_ref()
                    .map(NativeSmoothStartupJob::pending_message)
            })
            .or_else(|| {
                self.local_startup_job
                    .as_ref()
                    .map(|_| "Подготовка local media...")
            })
            .or_else(|| {
                self.startup_playlist_pending
                    .then_some("Импорт startup playlist...")
            })
    }

    /// Сообщает shell scheduler-у, нужно ли продолжать polling startup jobs.
    pub(crate) fn has_pending_startup_job(&self) -> bool {
        self.yt_dlp_startup_job.is_some()
            || self.direct_media_startup_job.is_some()
            || self.native_hls_startup_job.is_some()
            || self.native_dash_startup_job.is_some()
            || self.native_hds_startup_job.is_some()
            || self.native_smooth_startup_job.is_some()
            || self.local_startup_job.is_some()
            || self.startup_playlist_pending
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
        app_state.begin_startup_readiness(StartupReadinessExpectation::new(
            StartupMediaOpenKind::Cli,
            StartupTargetExpectation::Beginning,
            if app_config.player.start_paused {
                StartupPlaybackExpectation::Paused
            } else {
                StartupPlaybackExpectation::Playing
            },
            StartupAudioExpectation::Unknown,
        ));

        if let InitialMedia::Playlist(path) = &initial_media {
            match playlist_runtime.start_startup_playlist_import(path.clone()) {
                Ok(true) => {
                    self.startup_playlist_pending = true;
                    app_state.set_startup_pending("Импорт startup playlist...".to_owned());
                }
                Ok(false) => {
                    self.orchestration.preparation_failed();
                    app_state.set_startup_error(
                        "Startup playlist import уже занят другим заданием".to_owned(),
                    );
                }
                Err(error) => {
                    self.orchestration.preparation_failed();
                    app_state.set_startup_error(error);
                }
            }
            return;
        }

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
            InitialMedia::Playlist(_) => {
                unreachable!("playlist startup intent returned before media admission")
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
            app_state.audio_decode_capability_snapshot(),
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
    app_config: &AppConfig,
    system_capabilities: &SystemCapabilities,
    audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    invocation_reason: web_media_core::ExtractorInvocationReason,
    cancellation: source_core::CancellationToken,
    is_cancelled: impl Fn() -> bool,
) -> Result<PreparedYtDlpStartupMedia> {
    let extractor_adapter = service_ytdlp::YtDlpExtractorAdapter::default();
    let prepared = crate::web_media_open::prepare_yt_dlp_web_media(
        source_locator,
        &app_config.network,
        &app_config.web_media,
        &app_config.yt_dlp,
        &extractor_adapter,
        &app_config.player.demux,
        &app_config.player.preferred_video_codec_order,
        system_capabilities,
        audio_capabilities,
        crate::web_media_open::YtDlpCandidateOpenIntent::BestPlayable,
        invocation_reason,
        cancellation,
        is_cancelled,
    )
    .context("Не удалось подготовить YtDlp media через candidate transport path")?;

    Ok(PreparedYtDlpStartupMedia {
        demuxer: prepared.demuxer,
        playlist_metadata: prepared.playlist_metadata,
        source_state: prepared.source_state,
        presentation: prepared.presentation,
        extractor_reason: prepared.extractor_reason,
        timeline_port: prepared.timeline_port,
        demux_seek_port: prepared.demux_seek_port,
        playback_window: prepared.playback_window,
        vod_endpoint_recovery: prepared.vod_endpoint_recovery,
    })
}

/// Выполняет generic direct media open chain без YtDlp-specific semantics.
pub(crate) fn resolve_direct_media_startup_media(
    source_locator: &service_direct_media::DirectMediaUrl,
    network_config: &NetworkConfig,
    demux_config: &PlayerDemuxConfig,
    cancellation: source_core::CancellationToken,
) -> Result<crate::direct_progressive_open::DirectProgressiveOpenResult> {
    crate::direct_progressive_open::open_direct_media(
        source_locator,
        network_config,
        demux_config,
        cancellation,
    )
    .context("Не удалось открыть direct media URL")
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

/// Распознаёт только утверждённые local playlist extensions без lossy path conversion.
fn is_recognized_startup_playlist_path(path: &Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| {
            ["m3u", "m3u8", "xspf", "cue"]
                .iter()
                .any(|recognized| extension.eq_ignore_ascii_case(recognized))
        })
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
            let native_path = PathBuf::from(native_argument);
            if is_recognized_startup_playlist_path(&native_path) {
                return (Some(InitialMedia::Playlist(native_path)), None);
            }
            return (Some(InitialMedia::File(native_path)), None);
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
        StartupUrlClassification::Unsupported { reason } => {
            return (None, Some(reason.safe_error()));
        }
    }

    // Всё остальное считаем локальным путём, как работало раньше.
    let local_path = PathBuf::from(utf8_argument);
    if is_recognized_startup_playlist_path(&local_path) {
        return (Some(InitialMedia::Playlist(local_path)), None);
    }
    (Some(InitialMedia::File(local_path)), None)
}

#[cfg(test)]
mod tests;
