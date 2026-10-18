//! Решение «что нарисовать на этом кадре»: описание кадра для субтитров и счётчики показа.
//!
//! Сам презентатор ([`SubtitlePresenter`]) лежит в `presenter`: он UI-фасад над потоком-
//! рисовальщиком (`render_worker`) и растеризатор не вызывает. Связь с потоком — `worker_link`.
//!
//! # Инварианты
//! - UI-поток не растеризует и не ждёт рисовальщика: готовая картинка берётся из очереди, а при
//!   промахе показывается последняя картинка без закончившихся реплик.
//! - Overlay хранится здесь (клон дешёвый: маски лежат в `Arc`), поэтому GPU-проход видит
//!   те же `Arc` масок и не загружает текстуру повторно.
//! - Ошибки отдельных реплик растеризатор гасит сам (пропуск + счётчик); фатальных путей,
//!   способных остановить воспроизведение, здесь нет.

mod ass_options;

mod ingest;
mod presenter;
mod retention_anchor;
#[cfg(test)]
pub(super) mod test_support;
pub(super) mod worker_link;

use std::time::Duration;

use render_core::{PictureLayout, PictureRect, PictureSize, RenderViewport, RenderableFrame};
use subtitle_raster::{AssStorageSize, TextLanguagePreference};
use video_present_core::VideoPresentFrameIdentity;

pub(super) use self::presenter::{PresenterSettings, SubtitlePresenter};

/// Кадр, который реально уходит в renderer, в терминах нужных субтитрам.
///
/// Собирается в точке submit из того, что уже известно там: identity кадра (его нет у
/// stale-lease, тогда субтитры скрыты) и геометрия. Поле `frame_pts` — pts именно
/// отправляемого кадра, а не позиция плеера: на паузе кадр тот же, значит и реплика та же;
/// после seek первый новый кадр сразу даёт новую реплику.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SubmittedFrameForSubtitles {
    frame_pts: Duration,
    render_generation: u64,
    picture_rect: PictureRect,
    /// Пиксельный размер картинки видео (ориентированный размер кадра): ASS по нему
    /// масштабирует обводку и тень (`storage size` libass).
    picture_size: PictureSize,
    /// Зажатая область видео в пикселях окна: в ней рисуются субтитры (чёрные полосы
    /// вокруг картинки - её часть).
    video_viewport: RenderViewport,
}

impl SubmittedFrameForSubtitles {
    /// Собирает описание кадра или `None`, если субтитры на нём показывать нельзя:
    /// - нет identity (stale-lease: во время seek показывается старый кадр — реплику
    ///   скрываем, чтобы «старая» реплика исчезала сразу);
    /// - у картинки или области видео нулевой размер.
    ///
    /// Область видео зажимается к экрану так же, как это делает `Renderer::render_frame`
    /// (`RenderViewport::clamp_to_surface`), поэтому прямоугольник картинки совпадает с тем,
    /// что нарисует видео-проход.
    pub(crate) fn from_submission(
        frame_identity: Option<VideoPresentFrameIdentity>,
        frame: &RenderableFrame,
        video_viewport: RenderViewport,
        screen_size_in_pixels: [u32; 2],
    ) -> Option<Self> {
        let frame_identity = frame_identity?;
        let clamped_viewport =
            video_viewport.clamp_to_surface(screen_size_in_pixels[0], screen_size_in_pixels[1]);
        let picture_size = PictureSize::from_frame(frame);
        let layout = PictureLayout::fit(picture_size, clamped_viewport)?;
        Some(Self {
            frame_pts: frame.pts,
            render_generation: frame_identity.render_generation(),
            picture_rect: layout.rect(),
            picture_size,
            video_viewport: clamped_viewport,
        })
    }

    /// Прямоугольник картинки видео на экране (без чёрных полос).
    pub(crate) fn picture_rect(&self) -> PictureRect {
        self.picture_rect
    }

    /// Размер картинки видео в пикселях как настройка ASS (`Unknown` для нулевой стороны).
    fn ass_storage_size(&self) -> AssStorageSize {
        AssStorageSize::from_pixels(self.picture_size.width, self.picture_size.height)
    }

    /// Зажатая область видео на экране: слой субтитров может стоять где угодно внутри неё.
    pub(super) fn video_viewport(&self) -> RenderViewport {
        self.video_viewport
    }

    /// Поколение ресурсов рендера, которому принадлежит кадр.
    pub(super) fn render_generation(&self) -> u64 {
        self.render_generation
    }

    /// Размер картинки видео, который уйдёт в ASS как `storage size` (для тестов геометрии).
    #[cfg(test)]
    pub(super) fn picture_size_for_test(&self) -> PictureSize {
        self.picture_size
    }

    /// Pts отправленного кадра (для тестов геометрии).
    #[cfg(test)]
    pub(super) fn frame_pts_for_test(&self) -> Duration {
        self.frame_pts
    }

    /// Собирает описание напрямую (тесты задают pts и геометрию без видео-кадра).
    #[cfg(test)]
    pub(super) fn for_test(
        frame_pts: Duration,
        render_generation: u64,
        picture_rect: PictureRect,
    ) -> Self {
        // Область видео по умолчанию - ровно картинка (без полос).
        let video_viewport = RenderViewport::new(
            picture_rect.x.max(0.0).floor() as u32,
            picture_rect.y.max(0.0).floor() as u32,
            picture_rect.width.ceil() as u32,
            picture_rect.height.ceil() as u32,
        );
        Self::for_test_with_viewport(frame_pts, render_generation, picture_rect, video_viewport)
    }

    /// То же, но с явной областью видео (тесты полос вокруг картинки).
    #[cfg(test)]
    pub(super) fn for_test_with_viewport(
        frame_pts: Duration,
        render_generation: u64,
        picture_rect: PictureRect,
        video_viewport: RenderViewport,
    ) -> Self {
        // В тестах размер картинки видео равен размеру её прямоугольника на экране.
        let picture_size = PictureSize::new(
            picture_rect.width.round() as u32,
            picture_rect.height.round() as u32,
        );
        Self {
            frame_pts,
            render_generation,
            picture_rect,
            picture_size,
            video_viewport,
        }
    }
}

/// Счётчики работы презентатора (для диагностики и тестов).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct PresentationStats {
    /// Сколько картинок нарисовал поток-рисовальщик (принятых UI).
    pub(super) rasterizer_calls: u64,
    /// Сколько кадров получили ту же точную картинку, что и предыдущий.
    pub(super) reused_frames: u64,
    /// Сколько кадров с репликами не нашли готовой картинки (показана предыдущая или никакой).
    pub(super) late_pictures: u64,
    /// Сколько готовых картинок выброшено при приёме: они нарисованы для сменившихся входов.
    pub(super) dropped_outdated_pictures: u64,
    /// Длительность последней растеризации.
    pub(super) last_rasterization: Duration,
    /// Самая долгая растеризация.
    pub(super) slowest_rasterization: Duration,
    /// Сколько реплик пропущено: их тип содержимого ещё не поддерживается.
    pub(super) skipped_unsupported_cues: u64,
    /// Языковое предпочтение текста в последнем запросе к растеризатору.
    pub(super) last_text_language: TextLanguagePreference,
}
