//! Вывод видео браузерного режима — шов между плеером и тем, кто показывает кадры.
//!
//! В 03b окна нет (решения 88, 92): [`HeadlessVideoOutput`] забирает кадры у плеера так
//! же, как рендер окна, правильно их освобождает и считает. В сессии 04 окно браузерного
//! режима реализует этот же trait своим рендером — и тем же движением получает выбор
//! декодера под рендер (VA-API без копий там, где рендер его принимает).
//!
//! Решение владельца сессии 03b: в 03b только программный FFmpeg. Аппаратный декод
//! требует импорта кадров рендером, а рендер приходит вместе с окном в 04.
//!
//! Что обязан делать вывод:
//! 1. сказать, что он умеет показывать ([`BrowserVideoOutput::capabilities`]) — по этому
//!    плеер решает, какой декодер годится для дорожки;
//! 2. запускать декодер по запросу ([`BrowserVideoOutput::start_backend`]): при старте —
//!    ДО загрузки медиа (поздняя замена декодера перематывает на текущую позицию, а
//!    у потока со страницы перемотки может не быть), и по запросу плеера под дорожку;
//! 3. забирать кадры показа по порядку и отпускать их обычным путём освобождения
//!    ([`BrowserVideoOutput::present`]).

use capability_core::{CapabilityScanner, SystemCapabilities};
use player_core::{
    PlayerVideoDecoderThreadConfig, PlayerWorker, StartedVideoBackend, VideoDecodeRequirement,
};
use render_core::RenderCapabilities;
use video_frame_contract::VideoFrameContract;
use video_present_core::VideoPresentFrameIdentity;

/// Для чего запускается декодер.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum VideoBackendStartRequest<'requirement> {
    /// Старт процесса: декодер по умолчанию ставится до первой загрузки медиа.
    Startup,
    /// Плеер выбрал дорожку, которую установленный декодер не тянет (или декодера нет).
    Stream(&'requirement VideoDecodeRequirement),
}

/// Итог запуска декодера.
pub(crate) enum VideoBackendStart {
    /// Декодер запущен; его нужно отдать плееру.
    Started(StartedVideoBackend),
    /// Подходящего декодера у этого вывода нет: видео дорожки не покажется.
    NoCompatibleBackend,
}

/// Декодер подходил, но не запустился (FFmpeg не поднялся, нет ресурсов).
#[derive(Debug, thiserror::Error)]
#[error("the video decoder did not start: {message}")]
pub(crate) struct VideoBackendStartError {
    /// Причина от backend-а (без адресов и заголовков — их здесь нет).
    message: String,
}

/// Что вывод сделал за один вызов [`BrowserVideoOutput::present`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PresentTally {
    /// Сколько новых (ещё не виденных) кадров забрано у плеера.
    pub(crate) taken_frames: u64,
}

/// Шов вывода видео. Реализуют: безоконный счётчик (03b) и окно с рендером (04).
pub(crate) trait BrowserVideoOutput: Send {
    /// Что вывод умеет показывать (провайдеры декодеров + форматы кадров рендера).
    fn capabilities(&self) -> SystemCapabilities;

    /// Запускает декодер под запрос. Не устанавливает его — это делает владелец плеера.
    fn start_backend(
        &mut self,
        request: VideoBackendStartRequest<'_>,
    ) -> Result<VideoBackendStart, VideoBackendStartError>;

    /// Забирает у плеера кадры показа, которые ещё не видел, и отпускает прежние.
    fn present(&mut self, player: &PlayerWorker) -> PresentTally;
}

/// Безоконный вывод: настоящий программный FFmpeg, кадры только считаются.
pub(crate) struct HeadlessVideoOutput {
    /// Лимиты потока декодера из config-а (потоки FFmpeg, очереди, пулы).
    decoder_thread_config: PlayerVideoDecoderThreadConfig,
    /// Последний забранный кадр: повторное чтение слота возвращает его же.
    last_taken_frame: Option<VideoPresentFrameIdentity>,
}

impl HeadlessVideoOutput {
    /// `decoder_thread_config` — тот же, что плеер собрал из config-а
    /// (`PlayerWorker::decoder_thread_config`): лимиты не захардкожены.
    pub(crate) fn new(decoder_thread_config: PlayerVideoDecoderThreadConfig) -> Self {
        Self {
            decoder_thread_config,
            last_taken_frame: None,
        }
    }

    /// Возможности «рендера» без окна: только программные кадры 4:2:0 8 бит.
    ///
    /// Формат кадров ограничен тем, что в 03b точно проверено «до кадра»; остальные
    /// программные форматы откроет рендер окна в 04 своими возможностями.
    fn headless_render_capabilities() -> RenderCapabilities {
        let mut render = RenderCapabilities::wgpu_nv12(None);
        render.display_name = "Headless frame counter (browser session)".to_string();
        render.supported_frame_contracts = vec![VideoFrameContract::host_yuv420_planar8()];
        render.advanced_ui = false;
        render.present_timing_metrics = false;
        render
    }

    /// Запускает программный FFmpeg с лимитами из config-а.
    fn start_software_decoder(&self) -> Result<StartedVideoBackend, VideoBackendStartError> {
        video_ffmpeg::FfmpegSoftwareVideoBackendFactory::new_with_decoder_config(
            self.decoder_thread_config,
        )
        .start_for_composition()
        .map_err(|error| VideoBackendStartError {
            message: format!("{error:#}"),
        })
    }
}

impl BrowserVideoOutput for HeadlessVideoOutput {
    fn capabilities(&self) -> SystemCapabilities {
        software_only_capabilities(Self::headless_render_capabilities())
    }

    fn start_backend(
        &mut self,
        request: VideoBackendStartRequest<'_>,
    ) -> Result<VideoBackendStart, VideoBackendStartError> {
        let compatible = match request {
            VideoBackendStartRequest::Startup => true,
            VideoBackendStartRequest::Stream(requirement) => self
                .capabilities()
                .playable_video_outputs
                .iter()
                .any(|output| output.satisfies(requirement)),
        };
        if !compatible {
            return Ok(VideoBackendStart::NoCompatibleBackend);
        }
        self.start_software_decoder()
            .map(VideoBackendStart::Started)
    }

    fn present(&mut self, player: &PlayerWorker) -> PresentTally {
        take_new_present_frames(player, &mut self.last_taken_frame)
    }
}

/// Сканирует возможности: только провайдер программного FFmpeg + данный «рендер».
pub(crate) fn software_only_capabilities(render: RenderCapabilities) -> SystemCapabilities {
    let mut scanner = CapabilityScanner::new();
    scanner.register_provider(Box::new(
        video_ffmpeg::FfmpegSoftwareCapabilityProvider::new(),
    ));
    scanner.register_render_capabilities(render);
    scanner.scan()
}

/// Забирает по порядку все кадры, которые плеер опубликовал с прошлого раза.
///
/// Плеер отдаёт кадры одной последовательности строго по порядку и держит незабранные
/// (`OrderedPresentFrameHandoff`); повторное чтение после последнего нового возвращает
/// уже забранный кадр — на нём цикл и останавливается. Каждый забранный кадр отмечается
/// как «отправленный в рендер» и сразу отпускается: последний clone lease-а уходит,
/// и освобождение идёт обычным путём RAII (`VideoFrameLeaseDropAck` → release sink
/// плеера → кадр возвращается декодеру), ровно как после показа окном.
fn take_new_present_frames(
    player: &PlayerWorker,
    last_taken_frame: &mut Option<VideoPresentFrameIdentity>,
) -> PresentTally {
    let mut tally = PresentTally::default();
    while let Some(lease) = player.try_acquire_present_frame() {
        let identity = VideoPresentFrameIdentity::from_decoded_frame(
            lease.render_generation(),
            lease.decoded_frame(),
        );
        if *last_taken_frame == Some(identity) {
            break;
        }
        lease.mark_submitted_to_renderer();
        *last_taken_frame = Some(identity);
        tally.taken_frames += 1;
    }
    tally
}
