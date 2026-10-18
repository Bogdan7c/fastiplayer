//! Размещение растровых субтитров (PGS) для `owner_placement`: признак «на экране картинки» и
//! расчёт сдвигов групп.
//!
//! Признак берётся у самого overlay (в нём есть картинки вида
//! [`SubtitleBitmapKind::Picture`]), а не из отдельного флага очереди: он не может разойтись с
//! тем, что реально нарисовано. Геометрия - в
//! [`super::super::layer_placement::picture_group`].

use subtitle_raster::{OverlayPlacementGroup, SubtitleBitmapKind, SubtitleOverlay};

use super::ResolvedPlacement;
use crate::state::subtitles::controls_obstruction::BottomControlsObstruction;
use crate::state::subtitles::layer_placement::picture_group::{
    PictureGroupPlacementRequest, place_picture_groups,
};
use crate::state::subtitles::presentation::SubmittedFrameForSubtitles;

/// На экране авторские растровые картинки (PGS).
///
/// ASS и растровые реплики на одной дорожке не встречаются, а образец режима правки всегда
/// текстовый, поэтому смешанный слой не возникает; на всякий случай ASS-правило проверяется
/// раньше (см. `placement_policy_for_shown_layer`).
pub(super) fn overlay_shows_pictures(overlay: &SubtitleOverlay) -> bool {
    overlay
        .bitmaps()
        .iter()
        .any(|bitmap| bitmap.kind() == SubtitleBitmapKind::Picture)
}

/// Сдвиги групп PGS-слоя: нижние картинки следуют вертикальному сдвигу пользователя и
/// поднимаются над панелью, надписи стоят на месте.
pub(super) fn resolve_picture_groups(
    overlay: &SubtitleOverlay,
    frame: &SubmittedFrameForSubtitles,
    vertical_offset_fraction: f32,
    obstruction: BottomControlsObstruction,
) -> Option<ResolvedPlacement> {
    let placement = place_picture_groups(&PictureGroupPlacementRequest {
        picture_rect: frame.picture_rect(),
        main_group_bounds: overlay.group_bounds(OverlayPlacementGroup::Main),
        fixed_group_bounds: overlay.group_bounds(OverlayPlacementGroup::Fixed),
        vertical_offset_fraction,
        obstruction,
    })?;
    Some(ResolvedPlacement {
        offsets: placement.offsets,
        rect: placement.rect,
    })
}
