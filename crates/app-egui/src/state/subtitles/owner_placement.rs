//! Размещение слоя субтитров на кадре: единая точка, где владелец превращает
//! «overlay растеризатора» в «overlay + сдвиг слоя».
//!
//! Владелец знает стиль пользователя (позиция) и наблюдение за нижней панелью управления,
//! чистая геометрия лежит в [`super::layer_placement`]. Растеризатор здесь не участвует:
//! смена позиции или подъём панели меняют только сдвиг, который рендер применяет через
//! uniform (без перерастеризации и без повторной загрузки масок).

mod ass_positioning;
mod picture_positioning;

use render_shell_api::SubtitleLayerOffsets;
use subtitle_raster::{OverlayPlacementGroup, SubtitleOverlay};

use super::active_track::ActiveSubtitleTrack;
use super::controls_obstruction::BottomControlsObstruction;
use super::layer_placement::group_lift::{MainGroupLiftRequest, lift_main_group_above_controls};
use super::layer_placement::{
    LayerBounds, LayerPlacement, LayerPlacementPolicy, LayerPlacementRequest, PlacedLayerRect,
    place_layer,
};
use super::media_state::MediaSubtitles;
use super::presentation::{SubmittedFrameForSubtitles, SubtitlePresenter};
use super::{CurrentMediaForSubtitles, SubtitleOwner};

pub(crate) use self::ass_positioning::AssLayerPositioning;

/// Overlay кадра вместе со сдвигом слоя, который должен применить рендер.
#[derive(Debug)]
pub(crate) struct PlacedSubtitleOverlay<'overlay> {
    /// Результат растеризатора (маски делятся между кадрами через `Arc`).
    pub(crate) overlay: &'overlay SubtitleOverlay,
    /// Сдвиги групп слоя относительно места, где растеризатор их нарисовал (обе группы
    /// получают один сдвиг, кроме режима «поднять основной диалог»).
    pub(crate) layer_offsets: SubtitleLayerOffsets,
}

/// Итог размещения кадра: сдвиги групп для рендера и прямоугольник слоя на экране.
struct ResolvedPlacement {
    offsets: SubtitleLayerOffsets,
    rect: PlacedLayerRect,
}

impl ResolvedPlacement {
    /// Весь слой едет одним сдвигом (обычные субтитры, `author`, `whole_layer`).
    fn whole_layer(placement: LayerPlacement) -> Self {
        Self {
            offsets: SubtitleLayerOffsets::uniform(placement.offset),
            rect: placement.rect,
        }
    }
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
    presenter.overlay_for_frame(media.displayed_track()?, frame)
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
    let real_track = media.displayed_track();
    match sample_track {
        Some(sample) => presenter.overlay_for_frame_or_sample(real_track, sample, frame),
        // Образец не собрался: правка продолжается с настоящими репликами, если они есть.
        None => real_track.and_then(|track| presenter.overlay_for_frame(track, frame)),
    }
}

/// Подъём основной группы над панелью: границы групп берутся у самого overlay.
fn resolve_main_group_lift(
    overlay: &SubtitleOverlay,
    frame: &SubmittedFrameForSubtitles,
    obstruction: BottomControlsObstruction,
) -> Option<ResolvedPlacement> {
    let lift = lift_main_group_above_controls(&MainGroupLiftRequest {
        picture_rect: frame.picture_rect(),
        main_group_bounds: overlay.group_bounds(OverlayPlacementGroup::Main),
        fixed_group_bounds: overlay.group_bounds(OverlayPlacementGroup::Fixed),
        obstruction,
    })?;
    Some(ResolvedPlacement {
        offsets: lift.offsets,
        rect: lift.rect,
    })
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
        let has_overlay = if self.appearance_edit.is_some() {
            gated_edit_overlay(
                self.media.as_ref(),
                self.sample_track.as_ref(),
                &mut self.presenter,
                current,
                frame,
            )
            .is_some()
        } else {
            gated_overlay(self.media.as_ref(), &mut self.presenter, current, frame).is_some()
        };
        let shown = self
            .presenter
            .shown_overlay()
            .filter(|_| has_overlay)
            .and_then(|overlay| Some((overlay, overlay.content_bounds()?)));
        let Some((overlay, content_bounds)) = shown else {
            // Слоя нет: рамке правки не на чем стоять.
            self.last_placed_layer_rect = None;
            self.last_placed_picture_rect = None;
            self.last_placed_layer_offset = None;
            return None;
        };
        // Правило размещения зависит от того, что именно показано (ASS, картинки PGS или
        // обычные субтитры).
        let policy = self.placement_policy_for_shown_layer(overlay);
        let placement = match policy {
            LayerPlacementPolicy::LiftMainGroupOnly => {
                resolve_main_group_lift(overlay, frame, obstruction)
            }
            LayerPlacementPolicy::MainGroupUserOffsetInsidePicture => {
                picture_positioning::resolve_picture_groups(
                    overlay,
                    frame,
                    vertical_offset_fraction,
                    obstruction,
                )
            }
            LayerPlacementPolicy::UserPlaced { .. } | LayerPlacementPolicy::AuthorPlaced => {
                place_layer(&LayerPlacementRequest {
                    picture_rect: frame.picture_rect(),
                    viewport: frame.video_viewport(),
                    content_bounds,
                    horizontal_offset_fraction,
                    vertical_offset_fraction,
                    obstruction,
                    policy,
                })
                .map(ResolvedPlacement::whole_layer)
            }
        };
        let Some(placement) = placement else {
            // Геометрия кадра нечисловая: показываем слой на стандартном месте, без сдвига.
            self.last_placed_layer_rect = None;
            self.last_placed_picture_rect = None;
            self.last_placed_layer_offset = None;
            return Some(PlacedSubtitleOverlay {
                overlay,
                layer_offsets: SubtitleLayerOffsets::NONE,
            });
        };
        self.last_placed_layer_rect = Some(placement.rect);
        self.last_placed_picture_rect = Some(frame.picture_rect());
        // Для правки на видео «видимое место» слоя - сдвиг основной группы (у остальных
        // режимов обе группы совпадают).
        self.last_placed_layer_offset = Some(placement.offsets.main);
        Some(PlacedSubtitleOverlay {
            overlay,
            layer_offsets: placement.offsets,
        })
    }

    /// Правило размещения для слоя, который показан сейчас.
    ///
    /// ASS привязан к картинке видео и на полосы не выходит (решение владельца, сессия 08):
    /// в режиме `main_lines_avoid_controls` над панелью поднимается только основной диалог, в
    /// режиме `author` он стоит там, где его нарисовал автор, в режиме `whole_layer` двигается
    /// целиком, но внутри картинки. Обычные субтитры размещаются по прежним правилам
    /// (в том числе образец режима правки, который никогда не является ASS). Растровый PGS
    /// размещает [`LayerPlacementPolicy::MainGroupUserOffsetInsidePicture`]; ASS-правило
    /// проверяется раньше него.
    ///
    /// В режиме правки при `author` и `main_lines_avoid_controls` и настоящей реплике ASS на кадре перетаскивание рамки
    /// ничего не двигает: правка двигает образец и обычные субтитры (образец появляется, когда
    /// реплики на кадре нет), а ASS остаётся на авторском месте.
    fn placement_policy_for_shown_layer(&self, overlay: &SubtitleOverlay) -> LayerPlacementPolicy {
        match self.shown_ass_layer_positioning() {
            // Растровые картинки (PGS), решение владельца сессии 11: нижние следуют вертикальному
            // сдвигу и подъёму над панелью, верхние стоят, всё внутри картинки видео.
            None if picture_positioning::overlay_shows_pictures(overlay) => {
                LayerPlacementPolicy::MainGroupUserOffsetInsidePicture
            }
            None => LayerPlacementPolicy::UserPlaced {
                bounds: LayerBounds::PictureAndBars,
            },
            Some(AssLayerPositioning::LiftMainLinesAboveControls) => {
                LayerPlacementPolicy::LiftMainGroupOnly
            }
            Some(AssLayerPositioning::KeepAuthorPlacement) => LayerPlacementPolicy::AuthorPlaced,
            Some(AssLayerPositioning::MoveWholeLayerInsidePicture) => {
                LayerPlacementPolicy::UserPlaced {
                    bounds: LayerBounds::PictureOnly,
                }
            }
        }
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
