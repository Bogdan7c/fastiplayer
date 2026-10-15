//! Сессия правки внешнего вида субтитров прямо на видео: типы намерений и чистая логика черновика.
//!
//! # Модель
//! Пока сессия открыта, пользователь меняет **черновик** (`draft`) стиля. Черновик применяется
//! вживую (владелец отдаёт его презентатору и размещению), но в конфиг не пишется до «Готово».
//! - `before` - закоммиченный стиль на момент входа (и обновляется вслед за конфигом, см.
//!   [`AppearanceEditSession::follow_committed_style`]); «Отмена» возвращает именно его.
//! - Позиция и размер хранятся ещё и в дробном виде ([`PreciseDraft`]): перетаскивание даёт
//!   десятые доли процента за кадр, и при округлении до целых каждый такой шаг терялся бы.
//!   `draft` всегда равен округлению дробных значений, а в конфиг уходит именно `draft`.
//!
//! Модуль ничего не знает о презентаторе, потоках и конфиге: это значения и чистые функции,
//! их владелец (`owner_appearance_edit`) связывает с остальной системой.

use fastiplayer_config::{
    MAX_BACKGROUND_OPACITY_PERCENT, MAX_POSITION_HORIZONTAL_PERCENT, MAX_POSITION_VERTICAL_PERCENT,
    MAX_TEXT_SIZE_PERCENT, MIN_POSITION_HORIZONTAL_PERCENT, MIN_POSITION_VERTICAL_PERCENT,
    MIN_TEXT_SIZE_PERCENT, SubtitleDecorationPreference,
};

use super::layer_placement::PlacedLayerRect;
use super::user_style::{
    SubtitlePosition, SubtitleRgb, UserSubtitleStyle, horizontal_offset_fraction_for_percent,
    position_percent_for_offset_fractions, vertical_offset_fraction_for_percent,
};

/// Сколько процентов в одной доле (доли картинки <-> проценты позиции).
const PERCENT_PER_FRACTION: f32 = 100.0;

/// Результат попытки начать правку.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppearanceEditBegin {
    /// Сессия открыта: черновик равен закоммиченному стилю.
    Started,
    /// Сессия уже была открыта; она не тронута.
    AlreadyActive,
    /// Видео нет: править внешний вид не на чем (кнопка входа должна быть выключена).
    NoMedia,
}

/// Одно изменение черновика. Все значения приводятся к допустимым диапазонам конфига.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum AppearanceEdit {
    /// Сдвиг слоя в долях КАРТИНКИ видео. Положительная вертикаль - ВВЕРХ (как растёт
    /// `position_vertical_percent`), положительная горизонталь - вправо. UI переводит
    /// экранный сдвиг мыши в доли и сам меняет знак вертикали.
    MoveBy {
        /// Доля ширины картинки.
        horizontal_fraction: f32,
        /// Доля высоты картинки.
        vertical_fraction: f32,
    },
    /// Поставить позицию точно (значения зажимаются в диапазоны конфига).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "точные значения: жесты UI шлют сдвиг и множитель, а точный ввод пока нужен только тестам владельца"
        )
    )]
    SetPosition(SubtitlePosition),
    /// Умножить размер текста (1.1 - на 10 % крупнее); итог в `50..=250` %.
    ScaleTextBy {
        /// Множитель; нечисловой или неположительный игнорируется.
        factor: f32,
    },
    /// Поставить размер текста в процентах пресета; итог в `50..=250`.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "точные значения: жесты UI шлют сдвиг и множитель, а точный ввод пока нужен только тестам владельца"
        )
    )]
    SetTextSize {
        /// Проценты пресета.
        percent: u16,
    },
    /// Цвет текста.
    SetTextColor(SubtitleRgb),
    /// Цвет плашки.
    SetBackgroundColor(SubtitleRgb),
    /// Непрозрачность плашки в процентах; итог в `0..=100`.
    SetBackgroundOpacity {
        /// Проценты.
        percent: u8,
    },
    /// Плашка или обводка.
    SetDecoration(SubtitleDecorationPreference),
}

/// Что произошло с черновиком после [`AppearanceEdit`]; не `bool`, чтобы «нет сессии» не
/// путалось с «значение не изменилось».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppearanceEditChange {
    /// Сессии нет: ничего не изменено.
    NotEditing,
    /// Черновик изменился.
    Changed,
    /// Сессия есть, но значение осталось прежним (уже на границе диапазона и т.п.).
    Unchanged,
}

/// Как завершить правку.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppearanceEditFinish {
    /// «Готово»: черновик нужно записать в конфиг.
    Save,
    /// Esc / «Отмена»: вернуть стиль, который был до входа.
    Cancel,
}

/// Итог завершения правки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppearanceEditOutcome {
    /// Сессии не было: ничего не сделано.
    NotEditing,
    /// Отмена: показ вернулся к закоммиченному стилю, в конфиг ничего не пишется.
    Cancelled,
    /// Сохранение запрошено: владелец положил стиль в очередь записи
    /// (`take_style_save_request`), запись выполняет вызывающий код.
    SaveRequested,
}

/// Снимок для UI правки: только значения, без ссылок на владельца.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AppearanceEditView {
    /// Черновик (то, что сейчас на экране).
    pub(crate) draft: UserSubtitleStyle,
    /// Где слой стоит на экране (пиксели окна); `None`, пока слоя ещё нет (шрифты грузятся).
    pub(crate) layer_rect: Option<PlacedLayerRect>,
    /// Картинка видео без полос (пиксели окна) на том же кадре, что и `layer_rect`; задана
    /// ровно тогда, когда задан он. Нужна, чтобы перевести сдвиг мыши в доли картинки.
    pub(crate) picture_rect: Option<render_core::PictureRect>,
}

/// Дробные значения черновика: позиция в процентах и размер текста в процентах.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PreciseDraft {
    horizontal_percent: f32,
    vertical_percent: f32,
    text_size_percent: f32,
}

impl PreciseDraft {
    /// Дробные значения, соответствующие целым значениям стиля.
    fn from_style(style: &UserSubtitleStyle) -> Self {
        Self {
            horizontal_percent: f32::from(style.position.horizontal_percent),
            vertical_percent: f32::from(style.position.vertical_percent),
            text_size_percent: f32::from(style.text_size_percent),
        }
    }

    /// Зажимает дробные значения в диапазоны конфига.
    fn clamped(self) -> Self {
        Self {
            horizontal_percent: self.horizontal_percent.clamp(
                f32::from(MIN_POSITION_HORIZONTAL_PERCENT),
                f32::from(MAX_POSITION_HORIZONTAL_PERCENT),
            ),
            vertical_percent: self.vertical_percent.clamp(
                f32::from(MIN_POSITION_VERTICAL_PERCENT),
                f32::from(MAX_POSITION_VERTICAL_PERCENT),
            ),
            text_size_percent: self.text_size_percent.clamp(
                f32::from(MIN_TEXT_SIZE_PERCENT),
                f32::from(MAX_TEXT_SIZE_PERCENT),
            ),
        }
    }
}

/// Открытая сессия правки.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct AppearanceEditSession {
    /// Закоммиченный стиль, к которому возвращает «Отмена».
    before: UserSubtitleStyle,
    /// Черновик, применённый вживую.
    draft: UserSubtitleStyle,
    /// Дробная точность позиции и размера (см. документацию модуля).
    precise: PreciseDraft,
}

impl AppearanceEditSession {
    /// Новая сессия: черновик равен закоммиченному стилю.
    pub(super) fn begin(committed: &UserSubtitleStyle) -> Self {
        Self {
            before: committed.clone(),
            draft: committed.clone(),
            precise: PreciseDraft::from_style(committed),
        }
    }

    /// Закоммиченный стиль, который вернёт «Отмена».
    pub(super) fn before(&self) -> &UserSubtitleStyle {
        &self.before
    }

    /// Черновик (округлённые значения, как они уйдут в конфиг).
    pub(super) fn draft(&self) -> &UserSubtitleStyle {
        &self.draft
    }

    /// Закоммиченный стиль изменился во время сессии (чужая транзакция настроек):
    /// `before` следует за ним, а черновик пользователя НЕ перезаписывается.
    pub(super) fn follow_committed_style(&mut self, committed: &UserSubtitleStyle) {
        self.before = committed.clone();
    }

    /// Сдвиг центра слоя от середины картинки в долях ширины с дробной точностью.
    pub(super) fn horizontal_offset_fraction(&self) -> f32 {
        horizontal_offset_fraction_for_percent(self.precise.horizontal_percent)
    }

    /// Сдвиг низа слоя от места пресета в долях высоты картинки с дробной точностью.
    pub(super) fn vertical_offset_fraction(&self) -> f32 {
        vertical_offset_fraction_for_percent(self.precise.vertical_percent)
    }

    /// Начало переноса мышью: позиция черновика становится той, где слой сейчас ВИДЕН.
    ///
    /// Сохранённая позиция и видимая расходятся, когда слой упёрся в край картинки или поднят
    /// над панелью управления. Без этого шага первые пиксели движения мыши «съедались» бы этой
    /// разницей и рамка залипала (найдено на ручной приёмке, решение владельца: рамка всегда
    /// идёт за мышью). Цена решения: позицию «на полосе под панелью» нельзя сохранить через
    /// повторный захват, пока панель видна, — только одним переносом вниз и «Готово».
    pub(super) fn start_move_from_shown_offset(
        &mut self,
        shown_horizontal_fraction: f32,
        shown_vertical_fraction: f32,
    ) -> AppearanceEditChange {
        if !shown_horizontal_fraction.is_finite() || !shown_vertical_fraction.is_finite() {
            return AppearanceEditChange::Unchanged;
        }
        let previous = (self.draft.clone(), self.precise);
        let (horizontal_percent, vertical_percent) = position_percent_for_offset_fractions(
            shown_horizontal_fraction,
            shown_vertical_fraction,
        );
        self.precise.horizontal_percent = horizontal_percent;
        self.precise.vertical_percent = vertical_percent;
        self.precise = self.precise.clamped();
        self.write_rounded_precise_into_draft();
        if (self.draft.clone(), self.precise) == previous {
            AppearanceEditChange::Unchanged
        } else {
            AppearanceEditChange::Changed
        }
    }

    /// «Сброс»: значения по умолчанию, кроме флага ASS (его нет на панели поверх видео).
    pub(super) fn reset_to_defaults(&mut self) -> AppearanceEditChange {
        let defaults = UserSubtitleStyle {
            ass_force_user_style: self.draft.ass_force_user_style,
            ..UserSubtitleStyle::default()
        };
        self.replace_draft(defaults)
    }

    /// Применяет одно изменение к черновику.
    pub(super) fn apply(&mut self, edit: AppearanceEdit) -> AppearanceEditChange {
        let previous = (self.draft.clone(), self.precise);
        match edit {
            AppearanceEdit::MoveBy {
                horizontal_fraction,
                vertical_fraction,
            } => self.move_by(horizontal_fraction, vertical_fraction),
            AppearanceEdit::SetPosition(position) => {
                self.precise.horizontal_percent = f32::from(position.horizontal_percent);
                self.precise.vertical_percent = f32::from(position.vertical_percent);
            }
            AppearanceEdit::ScaleTextBy { factor } => {
                if factor.is_finite() && factor > 0.0 {
                    self.precise.text_size_percent *= factor;
                }
            }
            AppearanceEdit::SetTextSize { percent } => {
                self.precise.text_size_percent = f32::from(percent);
            }
            AppearanceEdit::SetTextColor(color) => self.draft.text_color = color,
            AppearanceEdit::SetBackgroundColor(color) => self.draft.background_color = color,
            AppearanceEdit::SetBackgroundOpacity { percent } => {
                self.draft.background_opacity_percent = percent.min(MAX_BACKGROUND_OPACITY_PERCENT);
            }
            AppearanceEdit::SetDecoration(decoration) => self.draft.decoration = decoration,
        }
        self.precise = self.precise.clamped();
        self.write_rounded_precise_into_draft();
        if (self.draft.clone(), self.precise) == previous {
            AppearanceEditChange::Unchanged
        } else {
            AppearanceEditChange::Changed
        }
    }

    /// Сдвиг в долях картинки; нечисловые доли игнорируются.
    fn move_by(&mut self, horizontal_fraction: f32, vertical_fraction: f32) {
        if horizontal_fraction.is_finite() {
            self.precise.horizontal_percent += horizontal_fraction * PERCENT_PER_FRACTION;
        }
        if vertical_fraction.is_finite() {
            self.precise.vertical_percent += vertical_fraction * PERCENT_PER_FRACTION;
        }
    }

    /// Подменяет весь черновик целиком (сброс) вместе с дробными значениями.
    fn replace_draft(&mut self, draft: UserSubtitleStyle) -> AppearanceEditChange {
        let precise = PreciseDraft::from_style(&draft);
        let changed = draft != self.draft || precise != self.precise;
        self.draft = draft;
        self.precise = precise;
        if changed {
            AppearanceEditChange::Changed
        } else {
            AppearanceEditChange::Unchanged
        }
    }

    /// Округляет (уже зажатые) дробные значения в поля черновика.
    fn write_rounded_precise_into_draft(&mut self) {
        // Значения зажаты в диапазоны типов, поэтому приведение не теряет и не оборачивает.
        self.draft.position.horizontal_percent = self.precise.horizontal_percent.round() as u8;
        self.draft.position.vertical_percent = self.precise.vertical_percent.round() as i16;
        self.draft.text_size_percent = self.precise.text_size_percent.round() as u16;
    }
}
