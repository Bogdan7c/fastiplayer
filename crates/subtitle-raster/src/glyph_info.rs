//! Символы реплики с результатами шейпинга и раскладки (libass: `GlyphInfo`).
//!
//! libass хранит всё в одном массиве `GlyphInfo`: по одному элементу на символ реплики и
//! цепочку `next` для лишних глифов кластера. Здесь это [`TextCluster`]: «корневой» глиф,
//! список дополнительных и общие признаки символа. Корневой глиф есть всегда — даже у символа,
//! поглощённого лигатурой (у него `skip = true`, но контур и рамка всё равно загружаются,
//! и раскладка строк их учитывает, как в libass).
//!
//! Тип намеренно открытый внутри crate: стадии конвейера (шейпинг, раскладка, композиция)
//! изменяют разные поля одного и того же массива в строгом порядке libass, и общий владелец
//! данных здесь — сам конвейер, а не отдельная стадия.

use std::sync::Arc;

use harfrust::Script;
use skrifa::GlyphId;

use crate::event_fade::EventFade;
use crate::event_style::AxisPair;
use crate::fixed_point::Vector26;
use crate::fonts::FaceId;
use crate::glyph_render::GlyphOutline;
use crate::source_text::DrawingId;

/// Рамка глифа в 26.6 (libass: `ASS_Rect`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct GlyphBox {
    /// Левый край.
    pub(crate) x_min: i32,
    /// Верхний край.
    pub(crate) y_min: i32,
    /// Правый край.
    pub(crate) x_max: i32,
    /// Нижний край.
    pub(crate) y_max: i32,
}

/// Один глиф кластера.
#[derive(Debug, Clone)]
pub(crate) struct PlacedGlyph {
    /// Номер глифа в лице [`Self::face`].
    pub(crate) glyph: GlyphId,
    /// Лицо, из которого взят глиф.
    pub(crate) face: FaceId,
    /// Смещение пера после глифа, 26.6 в пикселях кадра.
    pub(crate) advance: Vector26,
    /// Смещение рисования относительно пера (позиционирование меток), 26.6.
    pub(crate) offset: Vector26,
    /// Позиция глифа: до композиции — относительно начала реплики, после — в пикселях кадра.
    pub(crate) pos: Vector26,
    /// Контур при размере 256 px; `None` — контур не загрузился (глиф не рисуется).
    pub(crate) outline: Option<Arc<GlyphOutline>>,
    /// Сдвиг контура по вертикали в единицах контура ДО масштаба (libass:
    /// `transform.offset.y`). Ненулевой только у рисунка: ставит его базовую линию (`\pbo`).
    pub(crate) outline_offset_y: f64,
    /// Рамка контура в пикселях кадра, 26.6.
    pub(crate) bbox: GlyphBox,
    /// Максимальный подъём лица в пикселях кадра, 26.6.
    pub(crate) ascent: i32,
    /// Максимальный спуск лица в пикселях кадра, 26.6.
    pub(crate) descent: i32,
}

impl PlacedGlyph {
    /// Глиф без шейпинга: номер известен, метрик и контура пока нет.
    pub(crate) fn unshaped(glyph: GlyphId, face: FaceId) -> Self {
        Self {
            glyph,
            face,
            advance: Vector26::default(),
            offset: Vector26::default(),
            pos: Vector26::default(),
            outline: None,
            outline_offset_y: 0.0,
            bbox: GlyphBox::default(),
            ascent: 0,
            descent: 0,
        }
    }
}

/// Перенос строки перед символом (libass: `linebreak`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LineStart {
    /// Продолжение строки.
    #[default]
    NotStarted,
    /// Строка началась из-за автоматического переноса.
    AfterSoftBreak,
    /// Строка началась после явного перевода строки.
    AfterHardBreak,
}

impl LineStart {
    /// Символ — первый на новой строке.
    pub(crate) fn begins_line(self) -> bool {
        self != Self::NotStarted
    }
}

/// Символ реплики и всё, что с ним сделал конвейер.
#[derive(Debug, Clone)]
pub(crate) struct TextCluster {
    /// Исходный символ (после зеркалирования для RTL в libass он не меняется при HarfBuzz).
    pub(crate) symbol: char,
    /// Индекс внешнего вида в [`crate::source_text::SourceText::looks`].
    pub(crate) look: usize,
    /// Затухание символа (libass: `info->fade`); участок берёт его у первого символа.
    pub(crate) fade: EventFade,
    /// Масштаб по осям (libass: `info->scale_x`/`scale_y` после `fix_glyph_scaling`):
    /// `font_size / 256`, умноженный на `\fscx`/`\fscy`. Переводит размеры шейпера и контура
    /// (запрос 256 px) в пиксели кадра.
    pub(crate) glyph_scale: AxisPair,
    /// Межбуквенный интервал после масштаба букв в 26.6 (libass: `hspacing_scaled`);
    /// прибавляется к продвижению кластера в `retrieve_glyphs`.
    pub(crate) letter_spacing_d6: i32,
    /// У символа задан ненулевой `\fsp` (libass: `info->hspacing != 0`): такой символ
    /// шейпится и переупорядочивается отдельным участком, лигатуры в нём выключены.
    pub(crate) has_letter_spacing: bool,
    /// Наклон `\fay` символа (libass: `info->fay`): сдвигает базовую линию следующих глифов
    /// ([`crate::layout`], `apply_baseline_shear`); сам контур наклоняет матрица глифа.
    pub(crate) baseline_shear: f64,
    /// Толщина обводки символа в пикселях кадра (libass: `info->border_x/y`); нужна измерению
    /// строк, которое не видит видов символов.
    pub(crate) border: AxisPair,
    /// Кластер — рисунок `\p`: один псевдоглиф без шейпинга и поиска шрифта (libass:
    /// `drawing_text.str`). У текста `None`.
    pub(crate) drawing: Option<DrawingId>,
    /// Лицо, выбранное для символа (libass: `face_index`).
    pub(crate) face: FaceId,
    /// Письменность символа (для разбиения на участки шейпинга).
    pub(crate) script: Script,
    /// Уровень вложенности BiDi.
    pub(crate) bidi_level: u8,
    /// Уровень абзаца BiDi, которому принадлежит символ (нужен для порядка на строке).
    pub(crate) paragraph_level: u8,
    /// Номер участка шейпинга (libass: `shape_run_id`).
    pub(crate) shape_run_id: u32,
    /// Здесь начинается новый участок единого стиля (libass: `starts_new_run`).
    pub(crate) starts_new_run: bool,
    /// Символ не рисуется (обрезанный пробел, поглощён лигатурой, невидимый).
    pub(crate) skip: bool,
    /// Обрезанный пробел (libass: `is_trimmed_whitespace`).
    pub(crate) is_trimmed_whitespace: bool,
    /// Перенос перед символом.
    pub(crate) line_start: LineStart,
    /// Корневой глиф кластера.
    pub(crate) root: PlacedGlyph,
    /// Остальные глифы кластера (лигатуры/метки порождают несколько глифов на символ).
    pub(crate) extra: Vec<PlacedGlyph>,
    /// Суммарное смещение пера кластера, 26.6.
    pub(crate) cluster_advance: Vector26,
}

impl TextCluster {
    /// Все глифы кластера с правом изменения.
    pub(crate) fn glyphs_mut(&mut self) -> impl Iterator<Item = &mut PlacedGlyph> {
        std::iter::once(&mut self.root).chain(self.extra.iter_mut())
    }

    /// Правый край рамки корневого глифа в пикселях 26.6 с учётом позиции (для переноса строк).
    pub(crate) fn right_edge(&self) -> i32 {
        self.root.bbox.x_max.saturating_add(self.root.pos.x)
    }

    /// Левый край рамки корневого глифа в пикселях 26.6 с учётом позиции.
    pub(crate) fn left_edge(&self) -> i32 {
        self.root.bbox.x_min.saturating_add(self.root.pos.x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_start_distinguishes_breaks() {
        assert!(!LineStart::NotStarted.begins_line());
        assert!(LineStart::AfterSoftBreak.begins_line());
        assert!(LineStart::AfterHardBreak.begins_line());
    }
}
