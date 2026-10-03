//! App-owned фоновый job native HLS: запускает чистую подготовку
//! (`media_source_open::native_startup::hls`) в отдельном потоке, публикует
//! результат через owner mailbox с пробуждением UI и при typed fallback-триггере
//! вызывает ровно один extractor fallback. Сама подготовка переехала в
//! `media-source-open` (session-07 выноса web-media); здесь остаются поток,
//! wake/mailbox, join, выбор стартовой позиции и сборка `PreparedStartupMedia`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use anyhow::{Result, anyhow};
use capability_core::SystemCapabilities;
use media_core::MediaTime;
use web_media_hls::HlsVodStartIntent;

// Прежние пути `crate::startup_media::native_hls::*` сохранены для app-потребителей
// (`media_open/preparation.rs`, `orchestration`): подготовка теперь живёт в crate-е.
pub(crate) use media_source_open::native_startup::hls::{
    NativeHlsAdmissionPort, NativeHlsAttempt, NativeHlsPreparationRequest, PreparedNativeHlsMedia,
    ProductionNativeHlsAdmissionPort,
};

use crate::app_wake::{
    AppWakePort, CompletionPublishError, OwnerMailboxReceiver, WakeDelivery, owner_mailbox,
};
use crate::media_open::NativeHlsUrl;
use crate::process_shutdown::{FinishedThreadJoin, join_finished_thread};
use crate::startup_media::orchestration::{PreparedStartupMedia, StartupMediaTarget};

/// Результат одного последовательного native-admission/extractor-fallback job-а.
type NativeHlsStartupResult = std::result::Result<PreparedStartupMedia, String>;

/// Caller-owned identity и start policy одного native HLS startup resolve.
struct NativeHlsStartupResolveRequest {
    source: NativeHlsUrl,
    fallback_locator: service_ytdlp::YtDlpMediaLocator,
    start: HlsVodStartIntent,
}

/// Фоновый CLI job выполняет native admission и только затем возможный extractor fallback.
pub(super) struct NativeHlsStartupJob {
    pending_message: String,
    result_receiver: OwnerMailboxReceiver<(), NativeHlsStartupResult>,
    pub(super) join_handle: Option<JoinHandle<()>>,
    pending_result: Option<NativeHlsStartupResult>,
    pub(super) cancellation_requested: Arc<AtomicBool>,
    pub(super) source_cancellation: source_core::CancellationToken,
}

impl NativeHlsStartupJob {
    pub(super) fn spawn(
        source: NativeHlsUrl,
        fallback_locator: service_ytdlp::YtDlpMediaLocator,
        start: HlsVodStartIntent,
        app_config: fastiplayer_config::AppConfig,
        system_capabilities: SystemCapabilities,
        audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
        wake_port: AppWakePort,
    ) -> std::result::Result<Self, String> {
        let (result_publisher, result_receiver) = owner_mailbox(wake_port);
        let cancellation_requested = Arc::new(AtomicBool::new(false));
        let worker_cancellation_requested = Arc::clone(&cancellation_requested);
        let source_cancellation = source_core::CancellationToken::new();
        let worker_source_cancellation = source_cancellation.clone();
        let join_handle = thread::Builder::new()
            .name("native-hls-startup-opener".to_string())
            .spawn(move || {
                let result = resolve_native_hls_startup_media(
                    NativeHlsStartupResolveRequest {
                        source,
                        fallback_locator,
                        start,
                    },
                    &app_config,
                    &system_capabilities,
                    audio_capabilities,
                    worker_source_cancellation,
                    || worker_cancellation_requested.load(Ordering::Acquire),
                )
                .map_err(|error| format!("{error:#}"));
                if worker_cancellation_requested.load(Ordering::Acquire) {
                    return;
                }
                match result_publisher.publish_completion(result) {
                    Ok(WakeDelivery::EventLoopClosed) => tracing::debug!(
                        "Event loop закрыт; native HLS terminal оставлен без wake retry"
                    ),
                    Ok(WakeDelivery::Armed | WakeDelivery::Coalesced) => {}
                    Err(CompletionPublishError::AlreadyPublished) => tracing::warn!(
                        "Native HLS startup opener попытался опубликовать второй terminal"
                    ),
                }
            })
            .map_err(|error| format!("Не удалось запустить native HLS startup opener: {error}"))?;
        Ok(Self {
            pending_message: "Проверка native HLS...".to_owned(),
            result_receiver,
            join_handle: Some(join_handle),
            pending_result: None,
            cancellation_requested,
            source_cancellation,
        })
    }

    pub(super) fn pending_message(&self) -> &str {
        &self.pending_message
    }

    pub(super) fn try_take_result(&mut self) -> Option<NativeHlsStartupResult> {
        let drain = self.result_receiver.drain();
        if drain.completion.is_some() {
            self.pending_result = drain.completion;
        }
        match join_finished_thread(&mut self.join_handle) {
            FinishedThreadJoin::Joined | FinishedThreadJoin::AlreadyJoined => {
                self.pending_result.take().or_else(|| {
                    drain.producer_disconnected_without_completion.then(|| {
                        Err("Native HLS startup opener завершился без результата".to_owned())
                    })
                })
            }
            FinishedThreadJoin::Panicked => {
                self.pending_result = None;
                Some(Err("Native HLS startup opener завершился panic".to_owned()))
            }
            FinishedThreadJoin::StillRunning => None,
        }
    }
}

impl super::StartupMediaController {
    /// Запускает один sequential CLI native admission job без параллельного extractor probe-а.
    pub(crate) fn start_native_hls_startup_job(
        &mut self,
        source: NativeHlsUrl,
        fallback_locator: service_ytdlp::YtDlpMediaLocator,
        app_state: &mut crate::state::AppState,
        app_config: &fastiplayer_config::AppConfig,
        system_capabilities: &SystemCapabilities,
    ) {
        if let Some(error) = self.startup_job_admission_error() {
            self.orchestration.preparation_failed();
            self.startup_error = Some(error.clone());
            app_state.set_startup_error(error);
            return;
        }
        app_state.set_startup_pending("Проверка native HLS...".to_owned());
        let start = match self.orchestration.target.as_ref() {
            Some(StartupMediaTarget::RestoredCurrent(target)) => match target.position() {
                crate::playlist_runtime::StartupPosition::KeepStart => HlsVodStartIntent::Beginning,
                crate::playlist_runtime::StartupPosition::Restore(position) => {
                    HlsVodStartIntent::RestoreOrBeginning(MediaTime::from_duration(position))
                }
            },
            Some(StartupMediaTarget::CliReplacement) | None => HlsVodStartIntent::Beginning,
        };
        match NativeHlsStartupJob::spawn(
            source,
            fallback_locator,
            start,
            app_config.clone(),
            system_capabilities.clone(),
            app_state.audio_decode_capability_snapshot(),
            self.wake_port.clone(),
        ) {
            Ok(job) => {
                self.startup_error = None;
                self.native_hls_startup_job = Some(job);
            }
            Err(error) => {
                self.orchestration.preparation_failed();
                tracing::warn!(error = %error, "Не удалось запустить native HLS startup opener");
                self.startup_error = Some(error.clone());
                app_state.set_startup_error(error);
            }
        }
    }
}

/// Выполняет native HLS admission и ровно один fallback внутри одного startup worker-а.
fn resolve_native_hls_startup_media(
    request: NativeHlsStartupResolveRequest,
    app_config: &fastiplayer_config::AppConfig,
    system_capabilities: &SystemCapabilities,
    audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    cancellation: source_core::CancellationToken,
    is_cancelled: impl Fn() -> bool,
) -> Result<PreparedStartupMedia> {
    let NativeHlsStartupResolveRequest {
        source,
        fallback_locator,
        start,
    } = request;
    let mut port = ProductionNativeHlsAdmissionPort::new(NativeHlsPreparationRequest {
        source: &source,
        expected_selection: None,
        network_config: &app_config.network,
        web_media_config: &app_config.web_media,
        demux_config: &app_config.player.demux,
        preferred_video_codec_order: &app_config.player.preferred_video_codec_order,
        system_capabilities,
        audio_capabilities,
        start,
        cancellation: cancellation.clone(),
    });
    match NativeHlsAdmissionPort::prepare(&mut port)? {
        NativeHlsAttempt::Prepared(prepared) => Ok(PreparedStartupMedia::NativeHls {
            source,
            prepared: Box::new(prepared),
        }),
        NativeHlsAttempt::RequiresExtractorFallback(trigger) => {
            let mut fallback_owner =
                crate::media_open::native_fallback::NativeWebFallbackOwner::before_installed(
                    fallback_locator,
                );
            let fallback = fallback_owner
                .claim(trigger)
                .map_err(|rejection| anyhow!("native HLS fallback rejected: {rejection:?}"))?;
            let (fallback_locator, invocation_reason) = fallback.into_parts();
            if !app_config.yt_dlp.enabled {
                return Err(anyhow!(
                    "native HLS admission requires extractor fallback ({invocation_reason:?}), но YtDlp отключён"
                ));
            }
            tracing::info!(
                ?invocation_reason,
                "CLI native HLS admission передан единственному YtDlp fallback"
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
