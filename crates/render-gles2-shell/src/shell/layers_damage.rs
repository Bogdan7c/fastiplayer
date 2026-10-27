//! Учёт «слои под интерфейсом не менялись» для частичного показа.
//!
//! Сборщик кадра `gles2-egui` может перерисовать только часть цели, если видео и субтитры
//! рисуют те же пиксели, что в прошлом показанном кадре. Знает это только рендер плеера: видео
//! определяется картинкой на слое и планом рисования (область, исключения, letterbox, поворот,
//! цвет), субтитры — подготовленным overlay и положением холста, всё вместе — размером экрана.
//!
//! Принцип: ошибиться в сторону «изменились» дёшево (полная перерисовка, как раньше), в сторону
//! «не менялись» — портит картинку. Поэтому любое сомнение — [`LayersDamage::Changed`]:
//! первый кадр, кадр после отказа, ожидающая таблица HDR→SDR (она устанавливается внутри
//! рисования и меняет цвет при том же запросе).

use gles2_egui::LayersDamage;
use render_core::RenderViewport;
use render_shell_api::SubtitleCanvasOrigins;

use crate::VideoDrawPlan;
use crate::video_import::ShownFrame;

/// Положение субтитров кадра (сам overlay сверяет слой субтитров).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct SubtitlePlacement {
    /// Углы холста; `None` — положение вычислить нельзя (субтитры не рисуются).
    pub(super) origins: Option<SubtitleCanvasOrigins>,
    /// Область, в которой разрешено рисовать субтитры (видимая часть видео).
    pub(super) visible_area: RenderViewport,
}

/// Видео кадра.
#[derive(Debug, Clone)]
pub(super) struct VideoLayerState {
    /// Картинка на видеослое.
    pub(super) shown: Option<ShownFrame>,
    /// План рисования (запрос слою).
    pub(super) plan: VideoDrawPlan,
}

/// Всё, что определяет пиксели слоёв кадра, кроме содержимого overlay.
#[derive(Debug, Clone)]
pub(super) struct LayersSnapshot {
    /// Размер экрана в пикселях.
    pub(super) screen_size: [u32; 2],
    /// Видео; `None` — кадр без видео.
    pub(super) video: Option<VideoLayerState>,
    /// Субтитры; `None` — кадр без субтитров.
    pub(super) subtitles: Option<SubtitlePlacement>,
}

/// Помнит слои последнего ПОКАЗАННОГО кадра.
#[derive(Debug, Default)]
pub(super) struct PresentedLayersTracker {
    last: Option<LayersSnapshot>,
}

impl PresentedLayersTracker {
    /// Изменились ли слои кадра `now` относительно прошлого показанного.
    ///
    /// - `subtitle_pixels_unchanged` — слой субтитров уже держит именно этот overlay (спросить
    ///   ДО рисования кадра);
    /// - `hdr_table_pending` — таблица HDR→SDR досчитывается: её установка меняет цвет без
    ///   изменения запроса.
    pub(super) fn damage_of(
        &self,
        now: &LayersSnapshot,
        subtitle_pixels_unchanged: bool,
        hdr_table_pending: bool,
    ) -> LayersDamage {
        let Some(last) = &self.last else {
            return LayersDamage::Changed;
        };
        if hdr_table_pending || last.screen_size != now.screen_size {
            return LayersDamage::Changed;
        }
        let video_unchanged = match (&last.video, &now.video) {
            (None, None) => true,
            (Some(before), Some(after)) => {
                before.shown.is_some()
                    && before.shown == after.shown
                    && before.plan.request() == after.plan.request()
            }
            _ => false,
        };
        let subtitles_unchanged = match (last.subtitles, now.subtitles) {
            (None, None) => true,
            (Some(before), Some(after)) => before == after && subtitle_pixels_unchanged,
            _ => false,
        };
        if video_unchanged && subtitles_unchanged {
            LayersDamage::Unchanged
        } else {
            LayersDamage::Changed
        }
    }

    /// Кадр показан: его слои — «прошлые» для следующего.
    pub(super) fn record_presented(&mut self, snapshot: LayersSnapshot) {
        self.last = Some(snapshot);
    }

    /// Кадр не показан или состояние слоёв неизвестно: следующий кадр перерисуется целиком.
    pub(super) fn forget(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use gles2_egui::LayersDamage;
    use render_core::RenderViewport;
    use render_shell_api::SubtitleCanvasOrigins;

    use super::{LayersSnapshot, PresentedLayersTracker, SubtitlePlacement};

    fn placement(x: f32) -> SubtitlePlacement {
        SubtitlePlacement {
            origins: Some(SubtitleCanvasOrigins {
                main: [x, 4.0],
                fixed: [x, 4.0],
            }),
            visible_area: RenderViewport::full_surface(64, 48),
        }
    }

    fn snapshot(screen: [u32; 2], subtitles: Option<SubtitlePlacement>) -> LayersSnapshot {
        LayersSnapshot {
            screen_size: screen,
            video: None,
            subtitles,
        }
    }

    fn tracker_after(shown: &LayersSnapshot) -> PresentedLayersTracker {
        let mut tracker = PresentedLayersTracker::default();
        tracker.record_presented(shown.clone());
        tracker
    }

    #[test]
    fn nothing_was_presented_yet_means_changed() {
        let tracker = PresentedLayersTracker::default();
        assert_eq!(
            tracker.damage_of(&snapshot([64, 48], None), true, false),
            LayersDamage::Changed
        );
    }

    #[test]
    fn identical_layers_are_unchanged_and_every_difference_is_changed() {
        let shown = snapshot([64, 48], Some(placement(10.0)));
        let tracker = tracker_after(&shown);
        assert_eq!(
            tracker.damage_of(&shown, true, false),
            LayersDamage::Unchanged
        );
        // Другое содержимое overlay.
        assert_eq!(
            tracker.damage_of(&shown, false, false),
            LayersDamage::Changed
        );
        // Холст сместился.
        assert_eq!(
            tracker.damage_of(&snapshot([64, 48], Some(placement(12.0))), true, false),
            LayersDamage::Changed
        );
        // Субтитры исчезли или появились.
        assert_eq!(
            tracker.damage_of(&snapshot([64, 48], None), true, false),
            LayersDamage::Changed
        );
        // Размер экрана.
        assert_eq!(
            tracker.damage_of(&snapshot([80, 48], Some(placement(10.0))), true, false),
            LayersDamage::Changed
        );
        // Таблица HDR досчитывается: цвет изменится внутри рисования при том же запросе.
        assert_eq!(tracker.damage_of(&shown, true, true), LayersDamage::Changed);
    }

    #[test]
    fn no_layers_at_all_stay_unchanged_and_forget_resets_the_memory() {
        let empty = snapshot([64, 48], None);
        let mut tracker = tracker_after(&empty);
        assert_eq!(
            tracker.damage_of(&empty, false, false),
            LayersDamage::Unchanged
        );
        tracker.forget();
        assert_eq!(
            tracker.damage_of(&empty, false, false),
            LayersDamage::Changed
        );
    }
}
