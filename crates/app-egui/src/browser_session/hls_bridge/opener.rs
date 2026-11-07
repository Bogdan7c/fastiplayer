//! Открыватель HLS-поколения: готовый native HLS (`media-source-open`) поверх
//! транспорта моста, в своём потоке, с результатом через почтовый ящик.
//!
//! `prepare()` native HLS блокирующий: он качает корневой плейлист, варианты и первые
//! сегменты — у моста каждый ресурс идёт кругом «плеер → страница → плеер». Главный
//! цикл процесса блокировать нельзя (часы страницы, пульс тени), поэтому открытие
//! идёт в потоке `bridge-hls-open`, как у обычного плеера (`NativeHlsStartupJob`), а
//! главный цикл опрашивает ящик неблокирующим [`HlsGenerationOpener::poll_open`].
//!
//! Чем владеет открыватель:
//! - входами открытия до старта потока (транспорт поколения, номерок корня, настройки);
//! - токеном отмены поколения: открыватель, уничтоженный до результата (поколение
//!   сменилось, сеанс кончился), отменяет открытие. После результата токен живёт в
//!   рантайме HLS — его уже не отменяем: поколение доигрывает принятое, а сеть ему
//!   перекрывает печать очереди (`seal`) у переводчика.
//!
//! Корень открытия — адрес-заглушка номерка `root` (`format_resource_address`): движок
//! видит его как обычную ссылку, а транспорт моста превращает обратно в номерок. Подпись
//! медиа — нейтральная «browser stream», ни адреса, ни домена (инвариант 7).

use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};

use browser_bridge_protocol::session::player::StreamRejection;
use browser_bridge_protocol::{ResourceHandle, format_resource_address};
use fastiplayer_config::{AppConfig, NetworkConfig, PlayerDemuxConfig, VideoCodec, WebMediaConfig};
use media_source_open::native_startup::hls::{
    ExternalHlsTransport, NativeHlsAdmissionPort, NativeHlsAttempt, NativeHlsNetworkRoute,
    NativeHlsPreparationRequest, PreparedNativeHlsMedia, ProductionNativeHlsAdmissionPort,
};
use media_source_open::native_web_source::hls::NativeHlsUrl;
use media_source_open::prepared_web_media::compose_prepared_web_media;
use media_source_open::safe_media_label::SafeMediaLabel;
use player_core::PreparedMedia;
use pushed_media_source::{ExternalResourceHandle, ResourceFetchRequests};
use source_core::{CancellationToken, HttpRequestTarget};
use tracing::{debug, error};
use web_media_core::WebMediaPresentationKind;
use web_media_hls::HlsVodStartIntent;

use super::capabilities::{
    BrowserDecodeCapabilities, DecodeCapabilitiesAnswer, DecodeCapabilitiesSource,
};
use super::open_failure::{
    HlsOpenRejection, rejection_for_fallback_trigger, rejection_for_preparation_error,
};
use super::sealed_end::SealAwareHlsDemuxer;
use super::transport::BridgeResourceTransport;
use crate::browser_session::session_media::BROWSER_STREAM_LABEL;

/// Имя потока открытия (видно в отладчике и `top -H`).
const OPENER_THREAD_NAME: &str = "bridge-hls-open";

/// Настройки открытия native HLS — те же разделы config, что у обычного плеера.
#[derive(Debug, Clone)]
pub(crate) struct HlsOpenSettings {
    /// `[network]`: таймауты, повторы, лимиты.
    network: NetworkConfig,
    /// `[web_media]`: выбор варианта (высота, HDR), live.
    web_media: WebMediaConfig,
    /// `[player.demux]`.
    demux: PlayerDemuxConfig,
    /// Порядок предпочтения видеокодеков пользователя.
    preferred_video_codec_order: Vec<VideoCodec>,
}

impl HlsOpenSettings {
    /// Снимок нужных разделов config процесса.
    pub(crate) fn from_app_config(config: &AppConfig) -> Self {
        Self {
            network: config.network.clone(),
            web_media: config.web_media.clone(),
            demux: config.player.demux,
            preferred_video_codec_order: config.player.preferred_video_codec_order.clone(),
        }
    }
}

/// Живое ли поколение: от этого зависит, исполнять ли позицию страницы перемоткой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HlsPresentation {
    /// Ролик (есть `EXT-X-ENDLIST`): перематывается портом движка.
    OnDemand,
    /// Эфир: скользящее окно, корень обновляется тем же транспортом.
    Live,
}

/// Открытое поколение, готовое для плеера.
pub(crate) struct OpenedHlsGeneration {
    /// Медиа для плеера: демуксер под обёрткой печати, порт перемотки, живая шкала.
    pub(crate) prepared: PreparedMedia,
    /// Ролик или эфир.
    pub(crate) presentation: HlsPresentation,
}

/// Что показал опрос открывателя.
pub(crate) enum HlsOpenProgress {
    /// Ещё открывается (или ждёт возможностей плеера).
    Pending,
    /// Открылось.
    Ready(Box<OpenedHlsGeneration>),
    /// Не откроется: честный отказ поколения.
    Failed(HlsOpenRejection),
}

/// Всё, что нужно потоку открытия.
struct OpenerInputs {
    /// Транспорт моста поколения.
    transport: BridgeResourceTransport,
    /// Очередь поколения — для обёртки печати.
    requests: ResourceFetchRequests,
    /// Номерок корневого плейлиста.
    root: ExternalResourceHandle,
    /// Настройки открытия.
    settings: Arc<HlsOpenSettings>,
}

/// Где открыватель в жизни.
enum OpenerState {
    /// Поток ещё не запущен: ждём первого опроса (и возможностей плеера).
    NotStarted(Box<OpenerInputs>),
    /// Поток открывает; результат придёт в ящик.
    Opening {
        /// Ящик результата.
        outcome: Receiver<Result<OpenedHlsGeneration, HlsOpenRejection>>,
        /// Поток (присоединяется после результата, чтобы не висеть на нём).
        worker: Option<JoinHandle<()>>,
    },
    /// Результат отдан.
    Finished,
}

/// Открыватель одного HLS-поколения.
pub(crate) struct HlsGenerationOpener {
    /// Состояние.
    state: OpenerState,
    /// Отмена открытия (и жизнь рантайма HLS после него).
    cancellation: CancellationToken,
}

impl HlsGenerationOpener {
    /// Открыватель поколения с корнем `root`; поток стартует на первом опросе.
    pub(crate) fn new(
        transport: BridgeResourceTransport,
        requests: ResourceFetchRequests,
        root: ExternalResourceHandle,
        settings: Arc<HlsOpenSettings>,
    ) -> Self {
        Self {
            state: OpenerState::NotStarted(Box::new(OpenerInputs {
                transport,
                requests,
                root,
                settings,
            })),
            cancellation: CancellationToken::new(),
        }
    }

    /// Двигает открытие, не блокируя.
    ///
    /// Первый опрос с известными возможностями плеера запускает поток; пока окно не
    /// назвало возможности — `Pending`. Результат отдаётся ровно один раз.
    pub(crate) fn poll_open(
        &mut self,
        capabilities: &dyn DecodeCapabilitiesSource,
    ) -> HlsOpenProgress {
        match std::mem::replace(&mut self.state, OpenerState::Finished) {
            OpenerState::NotStarted(inputs) => match capabilities.decode_capabilities() {
                DecodeCapabilitiesAnswer::NotYetKnown => {
                    self.state = OpenerState::NotStarted(inputs);
                    HlsOpenProgress::Pending
                }
                DecodeCapabilitiesAnswer::Known(known) => self.spawn(*inputs, *known),
            },
            OpenerState::Opening { outcome, worker } => self.poll_outcome(outcome, worker),
            OpenerState::Finished => {
                // Звать после результата — дефект владельца; честный отказ вместо паники.
                error!("The HLS generation opener was polled after its result");
                HlsOpenProgress::Failed(internal_failure())
            }
        }
    }

    /// Запускает поток открытия.
    fn spawn(
        &mut self,
        inputs: OpenerInputs,
        capabilities: BrowserDecodeCapabilities,
    ) -> HlsOpenProgress {
        let (sender, outcome) = mpsc::sync_channel(1);
        let cancellation = self.cancellation.clone();
        let spawned = thread::Builder::new()
            .name(OPENER_THREAD_NAME.to_owned())
            .spawn(move || {
                let result = open_generation(inputs, &capabilities, cancellation);
                // Получатель мог уйти (поколение сменилось): результат никому не нужен.
                if sender.send(result).is_err() {
                    debug!("The HLS generation result arrived after the generation was dropped");
                }
            });
        match spawned {
            Ok(worker) => {
                self.state = OpenerState::Opening {
                    outcome,
                    worker: Some(worker),
                };
                HlsOpenProgress::Pending
            }
            Err(spawn_error) => {
                error!(%spawn_error, "The HLS generation opener thread did not start");
                HlsOpenProgress::Failed(internal_failure())
            }
        }
    }

    /// Смотрит в ящик результата.
    fn poll_outcome(
        &mut self,
        outcome: Receiver<Result<OpenedHlsGeneration, HlsOpenRejection>>,
        mut worker: Option<JoinHandle<()>>,
    ) -> HlsOpenProgress {
        let result = match outcome.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => {
                self.state = OpenerState::Opening { outcome, worker };
                return HlsOpenProgress::Pending;
            }
            Err(TryRecvError::Disconnected) => {
                // Поток ушёл без результата: паника внутри подготовки.
                error!("The HLS generation opener thread ended without a result");
                Err(internal_failure())
            }
        };
        // Результат отправлен последним действием потока: присоединение не ждёт долго.
        if let Some(worker) = worker.take()
            && worker.join().is_err()
        {
            error!("The HLS generation opener thread panicked after its result");
        }
        match result {
            Ok(opened) => HlsOpenProgress::Ready(Box::new(opened)),
            Err(rejection) => HlsOpenProgress::Failed(rejection),
        }
    }
}

impl Drop for HlsGenerationOpener {
    /// Открыватель ушёл раньше результата — открытие никому не нужно: отмена.
    /// Поток не присоединяется (главный цикл не ждёт): он выйдет сам за ≤ 10 мс отмены.
    fn drop(&mut self) {
        if matches!(self.state, OpenerState::Opening { .. }) {
            self.cancellation.cancel();
        }
    }
}

impl fmt::Debug for HlsGenerationOpener {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let phase = match self.state {
            OpenerState::NotStarted(_) => "not_started",
            OpenerState::Opening { .. } => "opening",
            OpenerState::Finished => "finished",
        };
        formatter
            .debug_struct("HlsGenerationOpener")
            .field("phase", &phase)
            .finish_non_exhaustive()
    }
}

/// Тело потока: подготовка native HLS и сборка медиа для плеера.
fn open_generation(
    inputs: OpenerInputs,
    capabilities: &BrowserDecodeCapabilities,
    cancellation: CancellationToken,
) -> Result<OpenedHlsGeneration, HlsOpenRejection> {
    let OpenerInputs {
        transport,
        requests,
        root,
        settings,
    } = inputs;
    let source = root_source(root)?;
    let mut port = ProductionNativeHlsAdmissionPort::new(NativeHlsPreparationRequest {
        source: &source,
        expected_selection: None,
        network_config: &settings.network,
        web_media_config: &settings.web_media,
        demux_config: &settings.demux,
        preferred_video_codec_order: &settings.preferred_video_codec_order,
        system_capabilities: &capabilities.system,
        audio_capabilities: capabilities.audio,
        // Позицию страницы ставит общий механизм после загрузки (`AfterLoad`), как у файла.
        start: HlsVodStartIntent::Beginning,
        cancellation,
        network_route: NativeHlsNetworkRoute::External(ExternalHlsTransport::new(Arc::new(
            transport,
        ))),
    });
    match NativeHlsAdmissionPort::prepare(&mut port) {
        Ok(NativeHlsAttempt::Prepared(prepared)) => compose_for_player(prepared, requests),
        Ok(NativeHlsAttempt::RequiresExtractorFallback(trigger)) => {
            // У моста нет адреса для yt-dlp: «нужен извлекатель» — честный отказ.
            Err(rejection_for_fallback_trigger(trigger))
        }
        Err(preparation_error) => Err(rejection_for_preparation_error(&preparation_error)),
    }
}

/// Корень открытия: адрес-заглушка номерка с нейтральной подписью.
fn root_source(root: ExternalResourceHandle) -> Result<NativeHlsUrl, HlsOpenRejection> {
    let address = format_resource_address(ResourceHandle(root.get()));
    match HttpRequestTarget::parse_exact(address) {
        Ok(target) => Ok(NativeHlsUrl::new(
            target,
            SafeMediaLabel::from_service_safe_label(BROWSER_STREAM_LABEL),
        )),
        Err(target_error) => {
            // Адрес-заглушка всегда разбирается: это дефект договора, не данных страницы.
            error!(
                ?target_error,
                "The bridge root address is not a valid request target"
            );
            Err(internal_failure())
        }
    }
}

/// Подготовленный HLS → медиа для плеера (как `compose_native_hls_startup_media`
/// обычного плеера, но без восстановления адреса: его ведущего у моста нет).
fn compose_for_player(
    prepared: PreparedNativeHlsMedia,
    requests: ResourceFetchRequests,
) -> Result<OpenedHlsGeneration, HlsOpenRejection> {
    let PreparedNativeHlsMedia {
        demuxer,
        seek_port,
        // Каталог вариантов для меню версий звука — следующие сессии (10a2+).
        source_state: _,
        lifecycle,
    } = prepared;
    let attachments = lifecycle.into_web_attachments(seek_port);
    if attachments.vod_endpoint_recovery.is_some() {
        // Внешний маршрут восстановления не даёт; если даст — вести его некому.
        error!("The bridge HLS generation unexpectedly carries VOD endpoint recovery");
    }
    let presentation = match attachments.presentation {
        WebMediaPresentationKind::Vod => HlsPresentation::OnDemand,
        WebMediaPresentationKind::Live => HlsPresentation::Live,
    };
    let demuxer = Box::new(SealAwareHlsDemuxer::new(demuxer, requests));
    match compose_prepared_web_media(BROWSER_STREAM_LABEL, demuxer, attachments.prepared) {
        Ok(prepared) => Ok(OpenedHlsGeneration {
            prepared,
            presentation,
        }),
        Err(compose_error) => {
            error!(%compose_error, "The opened HLS generation could not be composed for the player");
            Err(internal_failure())
        }
    }
}

/// Отказ «дефект сборки, а не данных страницы».
const fn internal_failure() -> HlsOpenRejection {
    HlsOpenRejection {
        reason: StreamRejection::PlaybackFailed,
        class: "internal",
    }
}

#[cfg(test)]
#[path = "opener_tests.rs"]
mod tests;
