//! Намерение «как размещать слой ASS/SSA» для кода размещения (`owner_placement`).
//!
//! Решение владельца (субтитры, сессия 08): ASS всегда привязан к картинке видео, на чёрные
//! полосы не выходит. Настройка `subtitles.ass_position_mode` решает остальное:
//! - `main_lines_avoid_controls` (по умолчанию): основной диалог (без `\pos`/`\move`,
//!   снизу) плавно поднимается над панелью управления, надписи стоят, сдвиг пользователя к
//!   ASS не применяется;
//! - `author`: слой не двигается вообще (ни подъёма над панелью, ни сдвига пользователя),
//!   как в mpv;
//! - `whole_layer`: весь слой двигается как обычные субтитры (подъём над панелью + сдвиг
//!   пользователя), но не выходит за картинку.
//!
//! Модуль хранит и отдаёт это решение; геометрию применяет `place_layer` через
//! `LayerPlacementPolicy` (см. `owner_placement`).

use fastiplayer_config::AssPositionMode;

use crate::state::subtitles::SubtitleOwner;

/// Как приложение размещает слой ASS на картинке.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AssLayerPositioning {
    /// Основной диалог поднимается над панелью управления, надписи стоят, сдвига пользователя
    /// нет (режим по умолчанию).
    LiftMainLinesAboveControls,
    /// Слой остаётся там, где его нарисовал автор: сдвигов нет.
    KeepAuthorPlacement,
    /// Слой двигается целиком, как обычные субтитры, внутри картинки видео.
    MoveWholeLayerInsidePicture,
}

impl AssLayerPositioning {
    /// Намерение по значению конфига.
    pub(in crate::state::subtitles) fn from_config(mode: AssPositionMode) -> Self {
        match mode {
            AssPositionMode::MainLinesAvoidControls => Self::LiftMainLinesAboveControls,
            AssPositionMode::Author => Self::KeepAuthorPlacement,
            AssPositionMode::WholeLayer => Self::MoveWholeLayerInsidePicture,
        }
    }
}

impl SubtitleOwner {
    /// Правило размещения слоя ASS по настройке пользователя (не зависит от того, что
    /// сейчас на экране).
    pub(crate) fn ass_layer_positioning(&self) -> AssLayerPositioning {
        self.settings.ass_positioning
    }

    /// Правило размещения для показанного сейчас слоя: `Some`, если на экране реплики ASS,
    /// `None` для обычных субтитров (они размещаются по прежним правилам).
    pub(crate) fn shown_ass_layer_positioning(&self) -> Option<AssLayerPositioning> {
        self.presenter
            .shown_overlay_contains_ass()
            .then(|| self.ass_layer_positioning())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Все три значения конфига дают своё намерение.
    #[test]
    fn config_modes_map_to_intents() {
        assert_eq!(
            AssLayerPositioning::from_config(AssPositionMode::MainLinesAvoidControls),
            AssLayerPositioning::LiftMainLinesAboveControls
        );
        assert_eq!(
            AssLayerPositioning::from_config(AssPositionMode::Author),
            AssLayerPositioning::KeepAuthorPlacement
        );
        assert_eq!(
            AssLayerPositioning::from_config(AssPositionMode::WholeLayer),
            AssLayerPositioning::MoveWholeLayerInsidePicture
        );
    }
}
