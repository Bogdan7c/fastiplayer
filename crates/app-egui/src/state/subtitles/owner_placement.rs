//! Размещение слоя субтитров на кадре: единая точка, где владелец превращает
//! «overlay растеризатора» в «overlay + сдвиг слоя».
//!
//! Владелец знает стиль пользователя (позиция) и наблюдение за нижней панелью управления,
//! чистая геометрия лежит в [`super::layer_placement`]. Растеризатор здесь не участвует:
//! смена позиции или подъём панели меняют только сдвиг, который рендер применяет через
//! uniform (без перерастеризации и без повторной загрузки масок).

use render_wgpu_shell::SubtitleLayerOffset;
use subtitle_raster::SubtitleOverlay;

use super::active_track::ActiveSubtitleTrack;
use super::controls_obstruction::BottomControlsObstruction;
#[cfg(test)]
use super::layer_placement::PlacedLayerRect;
use super::layer_placement::{LayerPlacementRequest, place_layer};
use super::media_state::MediaSubtitles;
use super::presentation::{SubmittedFrameForSubtitles, SubtitlePresenter};
use super::track_list::TrackLoadState;
use super::{CurrentMediaForSubtitles, SubtitleOwner};

/// Overlay кадра вместе со сдвигом слоя, который должен применить рендер.
#[derive(Debug)]
pub(crate) struct PlacedSubtitleOverlay<'overlay> {
    /// Результат растеризатора (маски делятся между кадрами через `Arc`).
    pub(crate) overlay: &'overlay SubtitleOverlay,
    /// Сдвиг слоя относительно места, где растеризатор его нарисовал.
    pub(crate) layer_offset: SubtitleLayerOffset,
}

/// Overlay текущего кадра по правилам принадлежности (media и поколение ресурсов рендера).
///
/// Свободная функция над отдельными полями владельца: возвращаемая ссылка занимает только
/// презентатор, поэтому владелец может записать в свои другие поля, пока overlay жив.
pub(super) fn gated_overlay<'presenter>(
    media: Option<&MediaSubtitles>,
    presenter: &'presenter mut SubtitlePresenter,
    current: CurrentMediaForSubtitles,
    frame: &SubmittedFrameForSubtitles,
) -> Option<&'presenter SubtitleOverlay> {
    let media = media?;
    if current.media_instance_id != Some(media.media_instance_id)
        || frame.render_generation() != current.render_generation
    {
        return None;
    }
    let entry = media.tracks.get(media.display_track_id()?)?;
    let TrackLoadState::Ready(track) = &entry.load else {
        return None;
    };
    presenter.overlay_for_frame(track, frame)
}

/// Overlay кадра в режиме правки: те же правила принадлежности кадра media, но без требования
/// выбранной дорожки - при отсутствии настоящей реплики показывается образец.
fn gated_edit_overlay<'presenter>(
    media: Option<&MediaSubtitles>,
    sample_track: Option<&ActiveSubtitleTrack>,
    presenter: &'presenter mut SubtitlePresenter,
    current: CurrentMediaForSubtitles,
    frame: &SubmittedFrameForSubtitles,
) -> Option<&'presenter SubtitleOverlay> {
    let media = media?;
    if current.media_instance_id != Some(media.media_instance_id)
        || frame.render_generation() != current.render_generation
    {
        return None;
    }
    let real_track = media
        .display_track_id()
        .and_then(|track_id| media.tracks.get(track_id))
        .and_then(|entry| match &entry.load {
            TrackLoadState::Ready(track) => Some(track),
            _ => None,
        });
    match sample_track {
        Some(sample) => presenter.overlay_for_frame_or_sample(real_track, sample, frame),
        // Образец не собрался: правка продолжается с настоящими репликами, если они есть.
        None => real_track.and_then(|track| presenter.overlay_for_frame(track, frame)),
    }
}

impl SubtitleOwner {
    /// Принимает наблюдение за нижней панелью управления (UI вызывает каждый кадр).
    ///
    /// Только запоминает значение; расчёт подъёма идёт при подготовке overlay кадра,
    /// поэтому лишних вычислений на кадрах без субтитров нет.
    pub(in crate::state) fn observe_bottom_controls_obstruction(
        &mut self,
        obstruction: BottomControlsObstruction,
    ) {
        self.controls_obstruction = obstruction;
    }

    /// Прямоугольник слоя на экране после последнего размещения (пиксели окна).
    ///
    /// `None` - на последнем кадре субтитров не было. Нужен интерфейсу правки на видео,
    /// чтобы рисовать рамку ровно по слою.
    #[cfg(test)]
    pub(super) fn last_placed_layer_rect(&self) -> Option<PlacedLayerRect> {
        self.last_placed_layer_rect
    }

    /// Overlay кадра и сдвиг его слоя или `None`, если рисовать нечего.
    ///
    /// Правила принадлежности - в [`gated_overlay`]. Сдвиг считает [`place_layer`] по
    /// позиции из стиля пользователя и текущему наблюдению за панелью.
    pub(super) fn placed_overlay_for_frame(
        &mut self,
        current: CurrentMediaForSubtitles,
        frame: &SubmittedFrameForSubtitles,
    ) -> Option<PlacedSubtitleOverlay<'_>> {
        let (horizontal_offset_fraction, vertical_offset_fraction) = self.displayed_style_offsets();
        let obstruction = self.controls_obstruction;
        let overlay = if self.appearance_edit.is_some() {
            gated_edit_overlay(
                self.media.as_ref(),
                self.sample_track.as_ref(),
                &mut self.presenter,
                current,
                frame,
            )
        } else {
            gated_overlay(self.media.as_ref(), &mut self.presenter, current, frame)
        };
        let Some(overlay) = overlay else {
            // Слоя нет: рамке правки не на чем стоять.
            self.last_placed_layer_rect = None;
            self.last_placed_picture_rect = None;
            self.last_placed_layer_offset = None;
            return None;
        };
        let Some(content_bounds) = overlay.content_bounds() else {
            // Слоя нет: рамке правки не на чем стоять.
            self.last_placed_layer_rect = None;
            self.last_placed_picture_rect = None;
            self.last_placed_layer_offset = None;
            return None;
        };
        let placement = place_layer(&LayerPlacementRequest {
            picture_rect: frame.picture_rect(),
            viewport: frame.video_viewport(),
            content_bounds,
            horizontal_offset_fraction,
            vertical_offset_fraction,
            obstruction,
        });
        let Some(placement) = placement else {
            // Геометрия кадра нечисловая: показываем слой на стандартном месте, без сдвига.
            // Слоя нет: рамке правки не на чем стоять.
            self.last_placed_layer_rect = None;
            self.last_placed_picture_rect = None;
            self.last_placed_layer_offset = None;
            return Some(PlacedSubtitleOverlay {
                overlay,
                layer_offset: SubtitleLayerOffset::NONE,
            });
        };
        self.last_placed_layer_rect = Some(placement.rect);
        self.last_placed_picture_rect = Some(frame.picture_rect());
        self.last_placed_layer_offset = Some(placement.offset);
        Some(PlacedSubtitleOverlay {
            overlay,
            layer_offset: placement.offset,
        })
    }

    /// Только overlay (без сдвига): удобный вход для тестов растеризации и выбора реплик.
    #[cfg(test)]
    pub(super) fn overlay_for_frame(
        &mut self,
        current: CurrentMediaForSubtitles,
        frame: &SubmittedFrameForSubtitles,
    ) -> Option<&SubtitleOverlay> {
        gated_overlay(self.media.as_ref(), &mut self.presenter, current, frame)
    }
}
