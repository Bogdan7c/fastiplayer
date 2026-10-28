//! GLES2-рендер плеера за контрактом `render_shell_api::RenderShell` (сессия 09 плана
//! `user/gles2-renderer/`).
//!
//! # Кто чем владеет
//!
//! - [`Gles2Renderer`] — единственный владелец EGL-контекста и всех GL-объектов рендера
//!   (сборщик кадра с художником egui и маской углов, видеослой, слой субтитров). Приложение
//!   видит только `Box<dyn RenderShell>`, ключи кадров и билеты буфера.
//! - Показ на окне и модель отпуска окна — `presentation`; живые настройки цвета —
//!   `live_settings`; ожидание GPU — `gpu_idle`; слои кадра — `frame_layers`.
//! - Склад кадров декодера — `video_frames` (реализация `VideoFrameImport`) поверх
//!   [`crate::video_import`] (подготовка импорта, рецепт кадра в ключе, показ кадра на слое).
//! - Освобождение кадров декодера — только после fence кадра показа, потоком-ожидателем
//!   ([`crate::frame_release`], решения владельца 30 и 32). Рендер после КАЖДОГО показанного
//!   кадра ставит один fence и отдаёт его и видеослою, и привязке освобождения.
//!
//! # Порядок уничтожения (корневая причина падения при выходе, `results/07.md`, дефект 2)
//!
//! `Drop` уничтожает сборщик кадра, видеослой и слой субтитров, затем штатно отвязывает привязку
//! освобождения (`renderer_detaching`: все fence умирают в потоке рендера) и только потом
//! контекст. Привязку, которую принял преемник при пересоздании рендера, отвязывать нельзя.

mod create_error;
mod creation;
mod frame;
mod frame_fence;
mod frame_layers;
mod frame_outcome;
mod gpu_idle;
mod layers;
mod layers_damage;
mod live_settings;
mod presentation;
mod probe;
mod render_shell;
mod video_frames;

#[cfg(test)]
mod tests;

pub use probe::probe_gles2_renderer;

use gles2_egui::Gles2Context;
use render_core::{ColorPipelineSettings, HdrToSdrSettings, RenderCapabilities, RenderDiagnostics};
use render_shell_api::RendererInstanceId;

use self::layers::GlLayers;
use self::layers_damage::PresentedLayersTracker;
use self::live_settings::LiveSettingsState;
use self::presentation::PresentationController;
use crate::capabilities::HardwareZeroCopy;
use crate::host_block_import::{HostBlockZeroCopy, SoftwareZeroCopyStats};
use crate::video_import::{ActiveImport, ImportEpoch, OwnedReleaseBinding, ShownFrameTracker};

/// Счётчики кадров рендера за его жизнь (диагностика и приёмочные тесты).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Gles2FrameCounters {
    /// Показано кадров.
    pub frames_presented: u64,
    /// Кадров, где сборщик залил фон цели.
    pub background_fills: u64,
    /// Кадров, где заливку фона пропустили: видео само закрыло всю цель.
    pub background_fills_skipped: u64,
    /// Сколько раз картинку ключа пришлось вернуть на видеослой (ключ был не текущим).
    pub video_frames_reshown: u64,
    /// Кадров, где слои (видео, субтитры) не менялись с прошлого показанного кадра: на них
    /// возможна частичная перерисовка (`gles2_egui::LayersDamage::Unchanged`).
    pub frames_with_unchanged_layers: u64,
    /// Кадров с видео, не показанных из-за ошибки ПОСЛЕ начала рисования: их работа GPU всё равно
    /// прикрыта fence (кадр декодера не вернётся, пока GPU может его читать).
    pub failed_frames_fenced: u64,
    /// Сколько раз fence кадра поставить не удалось и пришлось звать `glFinish`.
    pub frame_fence_fallback_finishes: u64,
    /// Сколько раз не удалось ни fence, ни `glFinish` (контекст жив): кадры декодера, читаемые
    /// этой работой, могут вернуться раньше времени. Должно быть 0.
    pub frame_fence_failures: u64,
    /// Звонков в сигнал «проход показа отправлен» (12a.2): ровно один на показанный кадр.
    pub present_pass_signals: u64,
    /// Звонков, не дошедших до подписчиков (сигнал отравлен паникой). Должно быть 0.
    pub present_pass_signal_failures: u64,
}

/// GLES2-рендер окна приложения. Реализует `RenderShell`; живёт и используется в одном потоке
/// (контекст EGL привязан к потоку), переносимы только ключи кадров и подготовщик импорта.
pub struct Gles2Renderer {
    /// Паспорт экземпляра: им помечены билеты буфера и ключи кадров этого рендера.
    instance_id: RendererInstanceId,
    /// Что рендер умеет на этом драйвере (неизменно до пересоздания).
    capabilities: RenderCapabilities,
    /// Умеет ли драйвер аппаратный zero-copy (решает, примет ли рендер аппаратный импорт).
    hardware: HardwareZeroCopy,
    /// GL-объекты рендера; `None` только внутри `Drop` (после уничтожения).
    layers: Option<GlLayers>,
    /// Показ на окне (или модель окна у рендера без окна).
    presentation: PresentationController,
    /// Действующие живые настройки цвета.
    live_settings: LiveSettingsState,
    /// Нейтральная диагностика последнего кадра с видео (сбрасывается кадром без видео).
    diagnostics: RenderDiagnostics,
    /// Установленный импорт кадров декодера; `None` — декодер не подключён.
    active_import: Option<ActiveImport>,
    /// Эпоха последней установки импорта.
    import_epoch: ImportEpoch,
    /// Привязка освобождения кадров декодера по fence.
    release: OwnedReleaseBinding,
    /// Какой кадр сейчас на видеослое.
    shown: ShownFrameTracker,
    /// Слои последнего показанного кадра (решает, возможна ли частичная перерисовка).
    presented_layers: PresentedLayersTracker,
    /// Счётчики кадров.
    counters: Gles2FrameCounters,
    /// Сигнал «проход показа отправлен» для такта аппаратного декодера (12a.2).
    present_pass: video_core::PresentPassSignal,
    /// Программный кадр без CPU-копии (udmabuf): доступность, dma-buf блоков, счётчики.
    software_frames: HostBlockZeroCopy,
    /// Контекст EGL: объявлен последним и уничтожается после явной очистки в `Drop`.
    context: Gles2Context,
}

impl Gles2Renderer {
    /// Стартовые настройки цвета: та же семантика, что `render_wgpu_shell::Renderer::
    /// set_color_pipeline_settings` (без проверки «поддерживается ли поле», до первого кадра).
    pub fn set_color_pipeline_settings(&mut self, settings: ColorPipelineSettings) {
        self.live_settings.set_color_pipeline(settings);
    }

    /// Стартовые настройки HDR→SDR (см. [`Self::set_color_pipeline_settings`]).
    pub fn set_hdr_to_sdr_settings(&mut self, settings: HdrToSdrSettings) {
        self.live_settings.set_hdr_to_sdr(settings);
    }

    /// Счётчики кадров за жизнь рендера.
    #[must_use]
    pub const fn frame_counters(&self) -> Gles2FrameCounters {
        self.counters
    }

    /// Счётчики частичного показа за жизнь рендера: сколько кадров перерисовано частично, по каким
    /// причинам целиком и сколько пикселей перерисовано (доказательство выигрыша).
    #[must_use]
    pub fn partial_present_stats(&self) -> gles2_egui::PartialPresentStats {
        self.layers
            .as_ref()
            .map(|layers| layers.compositor.partial_present_stats())
            .unwrap_or_default()
    }

    /// Путь программного кадра без CPU-копии: состояние и счётчики (диагностика, тесты).
    #[must_use]
    pub const fn software_zero_copy_stats(&self) -> SoftwareZeroCopyStats {
        self.software_frames.stats()
    }

    /// Привязка освобождения кадров: статистика возвратов декодеру (диагностика).
    #[must_use]
    pub fn frame_release_stats(&self) -> crate::FrameReleaseStats {
        self.release.binding().stats()
    }
}

impl Drop for Gles2Renderer {
    fn drop(&mut self) {
        self.software_frames.log_summary();
        log::info!(
            "GLES2 partial present summary: {}; {}",
            self.partial_present_stats(),
            gles2_egui::describe_partial_present_support(self.context.capabilities())
        );
        if let Some(layers) = self.layers.take() {
            layers.destroy(&self.context);
        }
        // После уничтожения GL-объектов и ДО уничтожения контекста (см. документацию модуля).
        if let Some(report) = self.release.detach_for_shutdown()
            && (report.released_unfinished > 0 || report.waiter_still_holds_fence)
        {
            log::warn!(
                "GLES2: при закрытии рендера {} кадров декодера вернулись без fence \
                 (GPU не успел), поток освобождения держал fence: {}",
                report.released_unfinished,
                report.waiter_still_holds_fence
            );
        }
        // Итог жизни рендера для живых прогонов (сессия 12b): после отвязки освобождения, чтобы
        // в счётчики попали и кадры, возвращённые при закрытии. Сходимость возврата по fence
        // проверяется по `pending_now == 0` и нулевым `fence_wait_failures`/`frame_fence_failures`.
        log::info!(
            "GLES2 renderer lifetime summary: {:?}; {:?}",
            self.counters,
            self.release.binding().stats()
        );
    }
}
