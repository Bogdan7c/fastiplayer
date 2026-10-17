//! Теги караоке `\k`, `\K`, `\kf`, `\ko`, `\kt` в сыром виде (ass_parse.c, ветки `tag("kt")`,
//! `tag("kf") || tag("K")`, `tag("ko")`, `tag("k")`).
//!
//! Перевод в сотые доли секунды -> миллисекунды (`dtoi32(val * 10)`) и накопление времени по
//! слогам делает растеризатор: здесь только числа из файла.

/// Вид заливки слога.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssKaraokeKind {
    /// `\kf` и `\K` (регистр важен: `\K` — не то же, что `\k`): плавная заливка слева направо.
    Fill,
    /// `\ko`: до начала слога обводка скрыта.
    Outline,
    /// `\k`: слог меняет цвет скачком.
    Plain,
}

/// Тег `\k`/`\K`/`\kf`/`\ko`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AssKaraoke {
    pub kind: AssKaraokeKind,
    /// Длительность слога в сотых долях секунды «как написано» (`argtod`). `None` — тег без
    /// аргумента: libass берёт `100` (одна секунда).
    pub centiseconds: Option<f64>,
}
