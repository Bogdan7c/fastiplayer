//! Валидированные числа стиля: доли холста и размеры в em.
//!
//! Каждый тип хранит `f32` в разрешённом диапазоне и реализует `Eq + Hash` по битам числа —
//! это допустимо, потому что NaN и бесконечности в тип попасть не могут, а `-0.0` приводится
//! к `0.0`. Так стиль можно использовать как ключ кэша без потери точности сравнения.

/// Число вне разрешённого диапазона (или не конечное).
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
#[error("value {value} is outside the allowed range {min}..={max}")]
pub struct InvalidFraction {
    /// Отвергнутое значение.
    pub value: f32,
    /// Нижняя граница диапазона.
    pub min: f32,
    /// Верхняя граница диапазона.
    pub max: f32,
}

/// Объявляет тип-обёртку над `f32` с проверкой диапазона.
macro_rules! bounded_fraction {
    (
        $(#[$meta:meta])*
        $name:ident, $min:expr, $max:expr
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct $name(f32);

        impl $name {
            /// Нижняя граница допустимого диапазона.
            pub const MIN: f32 = $min;
            /// Верхняя граница допустимого диапазона.
            pub const MAX: f32 = $max;

            /// Проверяет диапазон; NaN и бесконечности отвергаются.
            pub fn new(value: f32) -> Result<Self, InvalidFraction> {
                if (Self::MIN..=Self::MAX).contains(&value) {
                    // `+ 0.0` превращает -0.0 в 0.0, иначе равные значения имели бы разные биты.
                    Ok(Self(value + 0.0))
                } else {
                    Err(InvalidFraction { value, min: Self::MIN, max: Self::MAX })
                }
            }

            /// Значение.
            pub fn get(self) -> f32 {
                self.0
            }

            /// Значение пресета, заданное константой в этом crate.
            ///
            /// Выход константы за диапазон — ошибка программиста; она закреплена тестом
            /// `presets_stay_inside_validated_ranges`, а в рантайме значение
            /// ограничивается диапазоном, чтобы не паниковать.
            pub(super) fn preset(value: f32) -> Self {
                Self(value.clamp(Self::MIN, Self::MAX))
            }
        }

        impl Eq for $name {}

        impl std::hash::Hash for $name {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                self.0.to_bits().hash(state);
            }
        }
    };
}

bounded_fraction!(
    /// Размер шрифта как доля высоты холста (0,01..=0,25): от «мелкого» до «на пол-экрана».
    FontSizeFraction,
    0.01,
    0.25
);

bounded_fraction!(
    /// Отступ как доля стороны холста (0..=0,5: больше половины стороны не имеет смысла).
    CanvasFraction,
    0.0,
    0.5
);

bounded_fraction!(
    /// Максимальная ширина строки как доля ширины холста (0,1..=1).
    LineWidthFraction,
    0.1,
    1.0
);

bounded_fraction!(
    /// Размер в em (долях размера шрифта): отступы плашки, обводка, тень (0..=2).
    EmSize,
    0.0,
    2.0
);
