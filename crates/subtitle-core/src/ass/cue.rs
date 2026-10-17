//! Содержимое одной реплики ASS: стиль, отступы и текст с блоками тегов.

use std::sync::Arc;

use super::overrides::AssOverride;
use super::script::{AssScript, AssStyleIndex};
use super::style::AssStyle;

/// Отступы реплики из полей `MarginL/R/V` события. `0` значит «взять из стиля»
/// (так делает libass), поэтому хранятся сырые числа.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AssEventMargins {
    pub left: i32,
    pub right: i32,
    pub vertical: i32,
}

/// Итоговые отступы после подстановки стиля.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssResolvedMargins {
    pub left: i32,
    pub right: i32,
    pub vertical: i32,
}

impl AssEventMargins {
    /// Подставляет отступ стиля вместо нулевого отступа события.
    pub fn resolve_with(self, style: &AssStyle) -> AssResolvedMargins {
        let pick = |event_value: i32, style_value: i32| {
            if event_value == 0 {
                style_value
            } else {
                event_value
            }
        };
        AssResolvedMargins {
            left: pick(self.left, style.margin_left),
            right: pick(self.right, style.margin_right),
            vertical: pick(self.vertical, style.margin_vertical),
        }
    }
}

/// Элемент текста реплики в порядке следования в исходной строке.
#[derive(Debug, Clone, PartialEq)]
pub enum AssTextItem {
    /// Обычный текст. Уже применены `\h` -> U+00A0, табуляция -> пробел, `\{` и `\}`
    /// -> литеральные скобки (`ass_get_next_char`).
    Text(String),
    /// `\N`: жёсткий перевод строки.
    HardBreak,
    /// `\n`: перевод строки только при итоговом режиме переноса «без переноса»
    /// (`\q2` / `WrapStyle: 2`), иначе пробел. Решает растеризатор — режим может
    /// поменяться тегом `\q` раньше по тексту.
    SoftBreak,
    /// Один блок `{...}` с разобранными тегами (пустые блоки не сохраняются).
    Overrides(Vec<AssOverride>),
}

/// Есть ли в СЫРОМ тексте реплики теги жёсткого позиционирования (`\pos`, `\move`, `\clip`,
/// `\iclip`, `\org`, `\pbo`, `\p`). Вычисляется при разборе по правилу libass
/// `ass_event_has_hard_overrides` (по сырой строке, а не по разобранным тегам: учитывает и
/// битые теги, и теги внутри `\t(...)`, и теги за лимитом числа тегов).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssHardOverrides {
    /// Хотя бы один такой тег найден.
    Present,
    /// Таких тегов нет.
    Absent,
}

/// Реплика ASS. Время и слой хранит `SubtitleEvent`; здесь — всё остальное.
#[derive(Debug, Clone, PartialEq)]
pub struct AssCue {
    /// Сценарий дорожки (общий для всех реплик).
    pub script: Arc<AssScript>,
    /// Стиль, найденный по имени из поля `Style`.
    pub style: AssStyleIndex,
    /// Порядковый номер события в потоке (`ReadOrder`; для файлов — номер в файле).
    pub read_order: i32,
    pub margins: AssEventMargins,
    /// Поле `Name`/`Actor`.
    pub name: String,
    /// Поле `Effect` (переходные эффекты Banner/Scroll пока не поддерживаются).
    pub effect: String,
    /// Жёсткие переопределения в сыром тексте (см. [`AssHardOverrides`]).
    pub hard_overrides: AssHardOverrides,
    pub text: Vec<AssTextItem>,
}

impl AssCue {
    /// Стиль реплики.
    pub fn style(&self) -> &AssStyle {
        self.script.style(self.style)
    }

    /// Отступы реплики с учётом стиля.
    pub fn resolved_margins(&self) -> AssResolvedMargins {
        self.margins.resolve_with(self.style())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_event_margin_means_use_style() {
        let style = AssStyle::libass_default();
        let margins = AssEventMargins {
            left: 0,
            right: 7,
            vertical: -3,
        };
        assert_eq!(
            margins.resolve_with(&style),
            AssResolvedMargins {
                left: style.margin_left,
                right: 7,
                vertical: -3,
            }
        );
    }
}
