//! Цвета ASS с семантикой libass.

/// Цвет стиля: RGB и ПРОЗРАЧНОСТЬ (не непрозрачность!).
///
/// В ASS и libass байт альфы инвертирован: `0x00` — полностью непрозрачный, `0xFF` —
/// полностью прозрачный. Поле называется `transparency`, чтобы перепутать было сложно;
/// для растеризатора есть [`AssColor::opacity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    /// `0` — непрозрачный, `255` — прозрачный (как в libass `_a()`).
    pub transparency: u8,
}

impl AssColor {
    pub const fn new(red: u8, green: u8, blue: u8, transparency: u8) -> Self {
        Self {
            red,
            green,
            blue,
            transparency,
        }
    }

    /// Привычная непрозрачность: `255` — видно полностью.
    pub const fn opacity(self) -> u8 {
        u8::MAX - self.transparency
    }

    /// Тот же цвет с другим байтом прозрачности.
    pub const fn with_transparency(self, transparency: u8) -> Self {
        Self {
            transparency,
            ..self
        }
    }

    /// RGB-часть без прозрачности.
    pub const fn rgb(self) -> AssRgb {
        AssRgb {
            red: self.red,
            green: self.green,
            blue: self.blue,
        }
    }
}

/// Цвет без прозрачности: значение тегов `\c`, `\1c`..`\4c` (прозрачность у них отдельная).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssRgb {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl AssRgb {
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    /// Из числа формата ASS `&HBBGGRR` (старший байт игнорируется).
    pub const fn from_bgr_value(value: u32) -> Self {
        Self {
            red: (value & 0xff) as u8,
            green: ((value >> 8) & 0xff) as u8,
            blue: ((value >> 16) & 0xff) as u8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opacity_is_inverse_of_transparency() {
        assert_eq!(AssColor::new(1, 2, 3, 0).opacity(), 255);
        assert_eq!(AssColor::new(1, 2, 3, 255).opacity(), 0);
        assert_eq!(AssColor::new(1, 2, 3, 0x80).opacity(), 0x7f);
    }

    #[test]
    fn bgr_value_puts_red_in_low_byte() {
        assert_eq!(AssRgb::from_bgr_value(0x0000_00ff), AssRgb::new(255, 0, 0));
        assert_eq!(
            AssRgb::from_bgr_value(0xff33_2211),
            AssRgb::new(0x11, 0x22, 0x33)
        );
    }

    #[test]
    fn with_transparency_keeps_rgb() {
        let color = AssColor::new(9, 8, 7, 0).with_transparency(0x40);
        assert_eq!(color, AssColor::new(9, 8, 7, 0x40));
        assert_eq!(color.rgb(), AssRgb::new(9, 8, 7));
    }
}
