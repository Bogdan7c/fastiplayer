//! Нейтральный словарь одного кадра: исходы, причины пропуска, тайминги стадий, интерфейс.
//!
//! Типы перенесены из `render-wgpu-shell/src/frame.rs` без изменения смысла (сессия 01 плана
//! `user/gles2-renderer/`). Имена стадий в [`RenderFrameStageTimings::slowest_stage`] — часть
//! диагностического контракта: их пишет трасса `render_frame_timing`, а читают инструменты замера.

use std::time::Duration;

/// Итог одного вызова `RenderShell::render_frame`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderFrameOutcome {
    /// Кадр был отправлен в swapchain и представлен.
    Presented(RenderFrameTiming),

    /// Кадр был пропущен из-за состояния surface/window.
    Dropped(RenderFrameDropReason),

    /// Video render path failed; caller must treat this as fatal media error.
    Failed(RenderFrameFailure),
}

/// Timing внутренних стадий одного `render_frame`.
///
/// Эти значения остаются на shell boundary: app/player layer видит только
/// длительности стадий, но не получает доступ к GPU surface/device/queue.
/// Рендер, у которого какой-то стадии нет (например, у GLES2 нет command encoder-а),
/// оставляет её нулевой: имя стадии не меняется, чтобы трассы разных рендеров сравнивались.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderFrameStageTimings {
    /// Обновление egui texture atlas-а перед созданием command buffer-а.
    pub egui_texture_update: Duration,

    /// Создание command encoder-а для текущего кадра.
    pub encoder_creation: Duration,

    /// Обновление egui vertex/index buffers.
    pub egui_buffer_update: Duration,

    /// Получение буфера swapchain (у wgpu — `surface.get_current_texture()`).
    pub surface_acquire: Duration,

    /// Создание view поверх полученной surface texture.
    pub surface_view_creation: Duration,

    /// Video pass или очистка target-а.
    pub video_render: Duration,

    /// Egui overlay pass поверх video output.
    pub egui_render: Duration,

    /// Overlay субтитров между video и egui.
    pub subtitle_overlay: Duration,

    /// Синхронная часть отправки работы GPU (у wgpu — `queue.submit(...)`).
    pub queue_submit: Duration,

    /// Неблокирующий опрос завершённой работы GPU (у wgpu — `device.poll(Poll)`).
    pub device_poll: Duration,

    /// Уведомление оконной системы перед present.
    pub pre_present_notify: Duration,

    /// Синхронная часть показа буфера (у wgpu — `queue.present`, у EGL — `eglSwapBuffers`).
    pub surface_present: Duration,
}

impl RenderFrameStageTimings {
    /// Возвращает сумму стадий submit/poll/present для старого aggregate counter-а.
    #[must_use]
    pub fn submit_present_elapsed(self) -> Duration {
        self.queue_submit + self.device_poll + self.pre_present_notify + self.surface_present
    }

    /// Возвращает самую дорогую стадию внутри renderer-owned части кадра.
    #[must_use]
    pub fn slowest_stage(self) -> RenderFrameSlowestStage {
        let mut slowest_stage = RenderFrameSlowestStage {
            name: "egui_texture_update",
            elapsed: self.egui_texture_update,
        };

        for candidate in [
            ("encoder_creation", self.encoder_creation),
            ("egui_buffer_update", self.egui_buffer_update),
            ("surface_acquire", self.surface_acquire),
            ("surface_view_creation", self.surface_view_creation),
            ("video_render", self.video_render),
            ("subtitle_overlay", self.subtitle_overlay),
            ("egui_render", self.egui_render),
            ("queue_submit", self.queue_submit),
            ("device_poll", self.device_poll),
            ("pre_present_notify", self.pre_present_notify),
            ("surface_present", self.surface_present),
        ] {
            if candidate.1 > slowest_stage.elapsed {
                slowest_stage = RenderFrameSlowestStage {
                    name: candidate.0,
                    elapsed: candidate.1,
                };
            }
        }

        slowest_stage
    }
}

/// Самая дорогая renderer-owned стадия кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderFrameSlowestStage {
    /// Стабильное имя стадии для logs/diagnostics.
    pub name: &'static str,

    /// Измеренная длительность стадии.
    pub elapsed: Duration,
}

/// Timing одного успешного render loop-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderFrameTiming {
    /// Время от отправки работы GPU до возврата из показа буфера.
    pub submit_present_elapsed: Duration,

    /// Полная длительность renderer-owned части от входа в shell до present.
    pub renderer_elapsed: Duration,

    /// Разбивка renderer-owned path по стадиям.
    pub stages: RenderFrameStageTimings,
}

impl RenderFrameTiming {
    /// Собирает итоговый timing и сохраняет старый aggregate submit/present counter.
    #[must_use]
    pub fn new(stages: RenderFrameStageTimings, renderer_elapsed: Duration) -> Self {
        Self {
            submit_present_elapsed: stages.submit_present_elapsed(),
            renderer_elapsed,
            stages,
        }
    }
}

/// Размер target-а и UI scale без раскрытия GPU-типов наружу.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderScreenDescriptor {
    /// Размер swapchain target-а в пикселях.
    pub size_in_pixels: [u32; 2],

    /// UI scale, полученный от egui context.
    pub pixels_per_point: f32,
}

/// Ошибка video render path, которую app/player layer не должен превращать в fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderFrameFailure {
    /// Что именно сломалось: от этого зависит, баг ли это вызывающего кода или рендера.
    pub kind: RenderFrameFailureKind,

    /// Сообщение renderer-а для логов и UI.
    pub message: String,
}

/// Класс ошибки кадра.
///
/// «Чужой билет» и «чужой ключ» — нарушения контракта вызывающим кодом (например, билет
/// старого рендера после пересоздания). Их нельзя путать с отказом GPU-пути рендера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFrameFailureKind {
    /// Рендер не смог записать или показать кадр (ошибка видео-пути, шейдера, драйвера).
    VideoRender,

    /// Буфер swapchain выдан не этим экземпляром рендера; кадр не рисовался.
    ForeignSurfaceFrame,

    /// Ключ видеокадра выдан не этим экземпляром рендера; кадр не рисовался.
    ForeignVideoFrame,
}

impl RenderFrameFailure {
    /// Создаёт failure из ошибки видео-пути рендера (прежняя семантика `new`).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            kind: RenderFrameFailureKind::VideoRender,
            message: message.into(),
        }
    }

    /// Отказ: билет на буфер swapchain выдан другим экземпляром рендера.
    #[must_use]
    pub fn foreign_surface_frame() -> Self {
        Self {
            kind: RenderFrameFailureKind::ForeignSurfaceFrame,
            message: "surface frame was acquired by another renderer instance".to_owned(),
        }
    }

    /// Отказ: ключ видеокадра подготовлен другим экземпляром рендера.
    #[must_use]
    pub fn foreign_video_frame() -> Self {
        Self {
            kind: RenderFrameFailureKind::ForeignVideoFrame,
            message: "video frame key was prepared by another renderer instance".to_owned(),
        }
    }
}

/// Причина пропуска кадра renderer backend-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFrameDropReason {
    /// Surface acquisition не успел завершиться.
    SurfaceTimeout,

    /// Окно occluded, compositor не принимает кадр.
    SurfaceOccluded,

    /// Surface потерян; renderer выполнил reconfigure и ждёт следующий redraw.
    SurfaceLost,

    /// Surface validation error при acquisition.
    SurfaceValidation,

    /// Повторный acquisition после Outdated/Reconfigure тоже не дал frame.
    SurfaceOutdatedRecoveryFailed,

    /// App попросил нарисовать интерфейс прежними примитивами, а удерживать нечего
    /// (первый кадр после создания renderer-а) или размер экрана изменился.
    /// Кадр отброшен до записи (полученный буфер swapchain возвращается без показа);
    /// app обязан повторить его полным egui-проходом.
    RetainedUiUnavailable,

    /// Рендер отпустил окно (`RenderShell::detach_presentation`, полное пересоздание рендера):
    /// буфера нет до `reattach_presentation`. Это не поломка surface, а шаг транзакции; кадр
    /// виден только если возврат окна не удался.
    PresentationDetached,

    /// Текстуры интерфейса рассинхронизированы (сессия 02a): частичная правка пришла для
    /// текстуры, которой у рендера нет (или которая в неё не помещается). Остальные изменения
    /// текстур кадра применены, кадр не рисовался (буфер swapchain возвращён без показа).
    /// Приложение обязано отправить полный набор текстур следующим кадром со свежим
    /// интерфейсом (`UiTextureSync::request_full_set`). Это не поломка surface.
    UiTexturesOutOfSync,
}

/// Чем рисовать интерфейс в этом кадре (cpu-parity, сессия 04).
pub enum EguiFrameContent {
    /// Свежий egui-проход: новые примитивы и изменения текстур этого кадра.
    Fresh {
        /// Уже tessellated egui primitives.
        paint_jobs: Vec<egui::epaint::ClippedPrimitive>,

        /// Изменения текстур интерфейса, накопленные с прошлой доставки
        /// (`UiTextureSync::take_delta_for_renderer`).
        ///
        /// Это изменение СОСТОЯНИЯ рендера, а не содержимое кадра (сессия 02a): рендер
        /// применяет `set` и `free`, даже если кадр затем отклонён (чужой билет или ключ,
        /// ошибка видео-пути, рассинхронизация текстур) — иначе приложение и рендер разойдутся,
        /// ведь приложение второй раз эти изменения не пришлёт.
        textures_delta: egui::TexturesDelta,
    },

    /// Интерфейс не менялся: нарисовать примитивы последнего свежего кадра поверх нового видео.
    ///
    /// Допустимо только при том же `RenderScreenDescriptor`, что у последнего свежего кадра;
    /// иначе рендер вернёт [`RenderFrameDropReason::RetainedUiUnavailable`] до записи кадра.
    ReusePrevious,
}

impl EguiFrameContent {
    /// Свежий кадр без примитивов и без изменений текстур (пустой интерфейс).
    ///
    /// Нужен тестам и стендам, которым важен только видео-путь.
    #[must_use]
    pub fn empty_fresh() -> Self {
        Self::Fresh {
            paint_jobs: Vec::new(),
            textures_delta: egui::TexturesDelta::default(),
        }
    }
}

impl std::fmt::Debug for EguiFrameContent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Примитивы egui большие; для логов достаточно вида и числа.
        match self {
            Self::Fresh { paint_jobs, .. } => formatter
                .debug_struct("Fresh")
                .field("paint_jobs", &paint_jobs.len())
                .finish_non_exhaustive(),
            Self::ReusePrevious => formatter.write_str("ReusePrevious"),
        }
    }
}

#[cfg(test)]
#[path = "tests/frame.rs"]
mod tests;
