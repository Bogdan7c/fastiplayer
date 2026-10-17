//! Раскладка полей строки `Dialogue:` (`Format:` секции `[Events]`).

/// Поле строки события. Имена распознаются без учёта регистра; `Actor` — синоним `Name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssEventField {
    /// `Layer` (в SSA — `Marked`, libass значение не читает и слой остаётся 0).
    Layer,
    Start,
    End,
    Style,
    Name,
    MarginL,
    MarginR,
    MarginV,
    Effect,
    /// Всегда последнее поле: забирает остаток строки вместе с запятыми.
    Text,
    /// Неизвестное или не читаемое libass поле (значение пропускается).
    Ignored,
}

/// Упорядоченный список полей строки события.
///
/// Хранится в [`AssScript`](super::AssScript), чтобы строки событий из пакетов контейнера
/// (где заголовок и события приходят отдельно) разбирались по той же раскладке.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssEventFormat {
    fields: Vec<AssEventField>,
}

impl AssEventFormat {
    pub fn new(fields: Vec<AssEventField>) -> Self {
        Self { fields }
    }

    /// Поля в порядке следования в строке.
    pub fn fields(&self) -> &[AssEventField] {
        &self.fields
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_field_order() {
        let format = AssEventFormat::new(vec![AssEventField::Start, AssEventField::Text]);
        assert_eq!(
            format.fields(),
            &[AssEventField::Start, AssEventField::Text]
        );
    }
}
