//! Вертикальные метрики шрифта «как у libass» (имитация GDI/VSFilter).
//!
//! Значения отдаются в единицах шрифта (font units); масштабирование на размер делает растеризатор.

use skrifa::FontRef;
use skrifa::raw::TableProvider;
use skrifa::raw::tables::os2::SelectionFlags;

/// Откуда взяты восходящая/нисходящая.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AscentDescentSource {
    /// OS/2 `usWinAscent`/`usWinDescent` (основной путь libass/GDI).
    WindowsOs2,
    /// Таблица `hhea` (то, что FreeType берёт по умолчанию).
    Hhea,
    /// OS/2 `sTypoAscender`/`sTypoDescender`.
    TypoOs2,
    /// Габаритный прямоугольник шрифта (`head.yMax`/`yMin`) — крайний случай libass.
    FontBoundingBox,
}

/// Метрики лица в font units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaceMetrics {
    /// Единиц шрифта на em.
    pub units_per_em: u16,
    /// Расстояние от базовой линии вверх (положительное).
    pub ascender: i32,
    /// Расстояние от базовой линии вниз как в FreeType: отрицательное или ноль.
    pub descender: i32,
    /// Межстрочное расстояние (ascender - descender [+ line gap]).
    pub height: i32,
    /// Откуда взята пара ascender/descender.
    pub source: AscentDescentSource,
}

impl FaceMetrics {
    /// По метрикам можно посчитать масштаб лица: ячейка строки положительна и `units_per_em`
    /// не нулевой. Лицо без этого не годится для рисования (см. `FaceScale::from_metrics`).
    pub fn is_renderable(&self) -> bool {
        self.units_per_em != 0 && i64::from(self.ascender) - i64::from(self.descender) > 0
    }
}

/// Считает метрики лица (libass 0.17.5: `ass_font.c:set_font_metrics`).
///
/// 1. Если `(i16)usWinAscent + (i16)usWinDescent != 0` — берём Win-значения:
///    ascender = winAscent, descender = -winDescent. Знаковое приведение сохранено
///    сознательно: часть шрифтов хранит в этих полях знаковые числа.
/// 2. Иначе стартуем со значений FreeType: `hhea`; если `hhea` нулевая — OS/2 typo, затем Win.
///    (FreeType предпочитает typo при флаге USE_TYPO_METRICS; повторено.)
/// 3. Если сумма ascender-descender или height равна нулю: typo из OS/2, если у него ненулевой
///    размах, иначе габариты шрифта `head.yMax`/`head.yMin`.
///
/// `None`, если нет таблицы `head` (шрифт непригоден).
pub fn compute_face_metrics(font: &FontRef<'_>) -> Option<FaceMetrics> {
    let head = font.head().ok()?;
    let units_per_em = head.units_per_em();
    let os2 = font.os2().ok();

    // Шаг 1: Windows-метрики OS/2.
    if let Some(os2) = &os2 {
        #[expect(
            clippy::cast_possible_wrap,
            reason = "libass сознательно читает u16 как short"
        )]
        let (win_ascent, win_descent) = (
            i32::from(os2.us_win_ascent() as i16),
            i32::from(os2.us_win_descent() as i16),
        );
        if win_ascent + win_descent != 0 {
            let ascender = win_ascent;
            let descender = -win_descent;
            return Some(FaceMetrics {
                units_per_em,
                ascender,
                descender,
                height: ascender - descender,
                source: AscentDescentSource::WindowsOs2,
            });
        }
    }

    // Шаг 2: значения, которые FreeType положил бы в face->ascender/descender/height.
    let (mut ascender, mut descender, mut height, mut source) =
        freetype_default_metrics(font, os2.as_ref());

    // Шаг 3: запасные варианты libass при вырожденных значениях.
    if ascender - descender == 0 || height == 0 {
        match &os2 {
            Some(os2)
                if i32::from(os2.s_typo_ascender()) - i32::from(os2.s_typo_descender()) != 0 =>
            {
                ascender = i32::from(os2.s_typo_ascender());
                descender = i32::from(os2.s_typo_descender());
                source = AscentDescentSource::TypoOs2;
            }
            _ => {
                ascender = i32::from(head.y_max());
                descender = i32::from(head.y_min());
                source = AscentDescentSource::FontBoundingBox;
            }
        }
        height = ascender - descender;
    }

    Some(FaceMetrics {
        units_per_em,
        ascender,
        descender,
        height,
        source,
    })
}

/// Пара ascender/descender/height в духе FreeType `sfobjs.c` (hhea с запасными таблицами).
fn freetype_default_metrics(
    font: &FontRef<'_>,
    os2: Option<&skrifa::raw::tables::os2::Os2<'_>>,
) -> (i32, i32, i32, AscentDescentSource) {
    let typo = os2.map(|table| {
        (
            i32::from(table.s_typo_ascender()),
            i32::from(table.s_typo_descender()),
            i32::from(table.s_typo_line_gap()),
        )
    });
    let prefers_typo = os2.is_some_and(|table| {
        table
            .fs_selection()
            .contains(SelectionFlags::USE_TYPO_METRICS)
    });
    if let (true, Some((ascender, descender, line_gap))) = (prefers_typo, typo)
        && ascender - descender != 0
    {
        return (
            ascender,
            descender,
            ascender - descender + line_gap,
            AscentDescentSource::TypoOs2,
        );
    }
    if let Ok(hhea) = font.hhea() {
        let ascender = i32::from(hhea.ascender().to_i16());
        let descender = i32::from(hhea.descender().to_i16());
        if ascender != 0 || descender != 0 {
            let line_gap = i32::from(hhea.line_gap().to_i16());
            return (
                ascender,
                descender,
                ascender - descender + line_gap,
                AscentDescentSource::Hhea,
            );
        }
    }
    if let Some((ascender, descender, line_gap)) = typo
        && (ascender != 0 || descender != 0)
    {
        return (
            ascender,
            descender,
            ascender - descender + line_gap,
            AscentDescentSource::TypoOs2,
        );
    }
    // FreeType тут берёт Win-значения; сюда мы попадаем только при нулевой сумме Win,
    // поэтому вырожденный результат уйдёт на запасной путь шага 3.
    (0, 0, 0, AscentDescentSource::Hhea)
}
