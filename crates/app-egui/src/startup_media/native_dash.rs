//! App-owned фоновый job native DASH: запускает чистую подготовку
//! (`media_source_open::native_startup::dash`) в отдельном потоке, публикует
//! результат через owner mailbox с пробуждением UI и при typed fallback-триггере
//! вызывает ровно один extractor fallback. Сама подготовка переехала в
//! `media-source-open` (session-07 выноса web-media); здесь остаются поток,
//! wake/mailbox, join и сборка `PreparedStartupMedia`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use anyhow::{Result, anyhow};
use source_core::CancellationToken;

// Прежние пути `crate::startup_media::native_dash::*` сохранены для app-потребителей
// (`media_open/preparation.rs`, `orchestration`): подготовка теперь живёт в crate-е.
pub(crate) use media_source_open::native_startup::dash::{
    NativeDashAttempt, NativeDashPreparationRequest, PreparedNativeDashMedia,
    prepare_native_dash_attempt,
};

use crate::app_wake::{
    AppWakePort, CompletionPublishError, OwnerMailboxReceiver, WakeDelivery, owner_mailbox,
};
use crate::media_open::NativeDashUrl;
use crate::process_shutdown::{FinishedThreadJoin, join_finished_thread};

use super::orchestration::PreparedStartupMedia;
use super::web_failure::StartupWebPreparationFailure;

/// Результат одного sequential native-admission/extractor-fallback startup job-а.
type NativeDashStartupResult =
    std::result::Result<PreparedStartupMedia, StartupWebPreparationFailure>;

/// Фоновый CLI job сначала завершает native content admission и только затем решает fallback.
pub(super) struct NativeDashStartupJob {
    /// Bounded pending label для startup overlay.
    pending_message: String,
    /// Exactly-once completion mailbox фонового resolver-а.
    result_receiver: OwnerMailboxReceiver<(), NativeDashStartupResult>,
    /// JoinHandle нужен bounded shutdown owner-у.
    pub(super) join_handle: Option<JoinHandle<()>>,
    /// Result удерживается до physical worker exit.
    pending_result: Option<NativeDashStartupResult>,
    /// Cooperative publication fence.
    pub(super) cancellation_requested: Arc<AtomicBool>,
    /// Тот же token физически отменяет HTTP/DASH work.
    pub(super) source_cancellation: CancellationToken,
}

impl NativeDashStartupJob {
    /// Запускает одну mutually-exclusive direct MPD attempt.
    pub(super) fn spawn(
        source: NativeDashUrl,
        fallback_locator: service_ytdlp::YtDlpMediaLocator,
        app_config: fastiplayer_config::AppConfig,
        system_capabilities: capability_core::SystemCapabilities,
        audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
        wake_port: AppWakePort,
    ) -> std::result::Result<Self, String> {
        let (result_publisher, result_receiver) = owner_mailbox(wake_port);
        let cancellation_requested = Arc::new(AtomicBool::new(false));
        let worker_cancellation_requested = Arc::clone(&cancellation_requested);
        let source_cancellation = CancellationToken::new();
        let worker_source_cancellation = source_cancellation.clone();
        let join_handle = thread::Builder::new()
            .name("native-dash-startup-opener".to_string())
            .spawn(move || {
                let result = resolve_native_dash_startup_media(
                    source,
                    fallback_locator,
                    &app_config,
                    &system_capabilities,
                    audio_capabilities,
                    worker_source_cancellation,
                    || worker_cancellation_requested.load(Ordering::Acquire),
                )
                .map_err(|error| StartupWebPreparationFailure::from_preparation_error(&error));
                if worker_cancellation_requested.load(Ordering::Acquire) {
                    return;
                }
                match result_publisher.publish_completion(result) {
                    Ok(WakeDelivery::EventLoopClosed) => tracing::debug!(
                        "Event loop закрыт; native DASH terminal оставлен без wake retry"
                    ),
                    Ok(WakeDelivery::Armed | WakeDelivery::Coalesced) => {}
                    Err(CompletionPublishError::AlreadyPublished) => tracing::warn!(
                        "Native DASH startup opener попытался опубликовать второй terminal"
                    ),
                }
            })
            .map_err(|error| format!("Не удалось запустить native DASH startup opener: {error}"))?;
        Ok(Self {
            pending_message: "Проверка native DASH...".to_owned(),
            result_receiver,
            join_handle: Some(join_handle),
            pending_result: None,
            cancellation_requested,
            source_cancellation,
        })
    }

    /// Возвращает safe pending label без locator material.
    pub(super) fn pending_message(&self) -> &str {
        &self.pending_message
    }

    /// Публикует result только после exact worker join.
    pub(super) fn try_take_result(&mut self) -> Option<NativeDashStartupResult> {
        let drain = self.result_receiver.drain();
        if drain.completion.is_some() {
            self.pending_result = drain.completion;
        }
        match join_finished_thread(&mut self.join_handle) {
            FinishedThreadJoin::Joined | FinishedThreadJoin::AlreadyJoined => {
                self.pending_result.take().or_else(|| {
                    drain.producer_disconnected_without_completion.then(|| {
                        Err(StartupWebPreparationFailure::unclassified(
                            "Native DASH startup opener завершился без результата",
                        ))
                    })
                })
            }
            FinishedThreadJoin::Panicked => {
                self.pending_result = None;
                Some(Err(StartupWebPreparationFailure::unclassified(
                    "Native DASH startup opener завершился panic",
                )))
            }
            FinishedThreadJoin::StillRunning => None,
        }
    }
}

/// Выполняет native content admission и единственный typed extractor fallback.
fn resolve_native_dash_startup_media(
    source: NativeDashUrl,
    fallback_locator: service_ytdlp::YtDlpMediaLocator,
    app_config: &fastiplayer_config::AppConfig,
    system_capabilities: &capability_core::SystemCapabilities,
    audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    cancellation: CancellationToken,
    is_cancelled: impl Fn() -> bool,
) -> Result<PreparedStartupMedia> {
    match prepare_native_dash_attempt(NativeDashPreparationRequest {
        source: &source,
        expected_selection: None,
        network_config: &app_config.network,
        web_media_config: &app_config.web_media,
        demux_config: &app_config.player.demux,
        system_capabilities,
        audio_capabilities,
        cancellation: cancellation.clone(),
    })? {
        NativeDashAttempt::Prepared(prepared) => Ok(PreparedStartupMedia::NativeDash {
            source,
            prepared: Box::new(prepared),
        }),
        NativeDashAttempt::RequiresExtractorFallback(trigger) => {
            let mut fallback_owner =
                crate::media_open::native_fallback::NativeWebFallbackOwner::before_installed(
                    fallback_locator,
                );
            let fallback = fallback_owner
                .claim(trigger)
                .map_err(|rejection| anyhow!("native DASH fallback rejected: {rejection:?}"))?;
            let (fallback_locator, invocation_reason) = fallback.into_parts();
            if !app_config.yt_dlp.enabled {
                // Типизированная причина (а не строка): классификатор покажет пользователю
                // «загрузка через yt-dlp отключена в настройках».
                return Err(anyhow::Error::new(service_ytdlp::YtDlpServiceError::AdapterDisabled)
                    .context(format!("native DASH admission requires extractor fallback ({invocation_reason:?})")));
            }
            tracing::info!(
                ?invocation_reason,
                "CLI native DASH admission передан единственному YtDlp fallback"
            );
            let prepared = super::resolve_yt_dlp_startup_media(
                &fallback_locator,
                app_config,
                system_capabilities,
                audio_capabilities,
                invocation_reason,
                cancellation,
                is_cancelled,
            )?;
            Ok(PreparedStartupMedia::Extractor {
                source_locator: fallback_locator,
                prepared: Box::new(prepared),
            })
        }
    }
}

impl super::StartupMediaController {
    /// Запускает один sequential CLI direct MPD job без параллельного extractor probe-а.
    pub(crate) fn start_native_dash_startup_job(
        &mut self,
        source: NativeDashUrl,
        fallback_locator: service_ytdlp::YtDlpMediaLocator,
        app_state: &mut crate::state::AppState,
        app_config: &fastiplayer_config::AppConfig,
        system_capabilities: &capability_core::SystemCapabilities,
    ) {
        if let Some(error) = self.startup_job_admission_error() {
            self.orchestration.preparation_failed();
            self.startup_error = Some(error.clone());
            app_state.set_startup_error(error);
            return;
        }
        app_state.set_startup_pending("Проверка native DASH...".to_owned());
        match NativeDashStartupJob::spawn(
            source,
            fallback_locator,
            app_config.clone(),
            system_capabilities.clone(),
            app_state.audio_decode_capability_snapshot(),
            self.wake_port.clone(),
        ) {
            Ok(job) => {
                self.startup_error = None;
                self.native_dash_startup_job = Some(job);
            }
            Err(error) => {
                self.orchestration.preparation_failed();
                tracing::warn!(error = %error, "Не удалось запустить native DASH startup opener");
                self.startup_error = Some(error.clone());
                app_state.set_startup_error(error);
            }
        }
    }
}
