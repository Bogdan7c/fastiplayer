//! Запрос шрифта и описание начертания: типизированные значения вместо голых `bool`/чисел.

/// Насыщенность: нам хватает двух классов (libass решает «нужна ли синтетика» по порогу веса).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontWeightClass {
    /// Обычное начертание.
    Regular,
    /// Полужирное (вес OS/2 от 550: libass 0.17.5 `ass_font_get_glyph` embolden-ит,
    /// если запрошенный вес 700 > вес шрифта + 150).
    Bold,
}

impl FontWeightClass {
    /// Класс по числовому весу OS/2 (`usWeightClass`).
    pub fn from_os2_weight(weight: u16) -> Self {
        // 700 > weight + 150  <=>  weight < 550 нуждается в синтетике.
        if weight >= 550 {
            Self::Bold
        } else {
            Self::Regular
        }
    }

    /// Типичный числовой вес класса (400 и 700): цель при выборе ближайшего по весу лица.
    pub fn nominal_os2_weight(self) -> u16 {
        match self {
            Self::Regular => 400,
            Self::Bold => 700,
        }
    }

    /// Полужирный ли класс.
    pub fn is_bold(self) -> bool {
        self == Self::Bold
    }
}

/// Наклон начертания.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontStyle {
    /// Прямое.
    Normal,
    /// Курсив или наклонное.
    Italic,
}

/// Нужно ли дорисовать жирность программно, потому что у выбранного шрифта нет Bold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyntheticBold {
    /// Начертание подошло как есть.
    NotNeeded,
    /// Запрошен Bold, а у шрифта его нет: контур надо утолщить.
    Needed,
}

/// Нужно ли наклонить глифы программно.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyntheticItalic {
    /// Начертание подошло как есть.
    NotNeeded,
    /// Запрошен курсив, а у шрифта его нет: глифы надо наклонить.
    Needed,
}

/// Запрос шрифта: семейство по имени (или `None` — шрифт по умолчанию) + начертание.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontRequest<'a> {
    /// Имя семейства или `None` для встроенного Noto Sans.
    pub family: Option<&'a str>,
    /// Желаемая насыщенность.
    pub weight: FontWeightClass,
    /// Желаемый наклон.
    pub style: FontStyle,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weight_threshold_matches_libass_embolden_rule() {
        assert_eq!(
            FontWeightClass::from_os2_weight(400),
            FontWeightClass::Regular
        );
        assert_eq!(
            FontWeightClass::from_os2_weight(549),
            FontWeightClass::Regular
        );
        assert_eq!(FontWeightClass::from_os2_weight(550), FontWeightClass::Bold);
        assert_eq!(FontWeightClass::from_os2_weight(700), FontWeightClass::Bold);
        assert!(FontWeightClass::Bold.is_bold());
    }
}
