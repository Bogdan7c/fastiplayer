//! «Подпись страницы»: домен и заголовок вкладки, из которой пришло видео (04c).
//!
//! Подпись нужна только окну браузерного режима: для заголовка окна и блока `Page` в
//! панели Info. Живёт строго в памяти окна. Инвариант 7: браузерный режим ничего не
//! пишет на диск и в журнал о просмотре, поэтому `Debug` типа скрыт — случайный
//! `?label` в `tracing` не выдаст, что смотрит человек.
//!
//! Тип намеренно свой, а не `PageIdentity` протокола: окно и `AppState` не должны
//! знать про протокол моста (контракт между слоями — «две строки для показа»).

use std::fmt;

/// Сколько символов (не байт) влезает в заголовок окна. Длиннее — обрезаем заголовок
/// страницы с многоточием: панель задач и рамка окна всё равно не покажут больше.
const MAX_WINDOW_TITLE_CHARS: usize = 100;

/// Разделитель заголовка страницы и домена в заголовке окна.
const TITLE_DOMAIN_SEPARATOR: &str = " — ";

/// Меньше стольких символов заголовка страницы показывать не имеет смысла: тогда
/// окно подписывается одним доменом (он коротко и точно говорит, откуда видео).
const MIN_USEFUL_TITLE_CHARS: usize = 8;

/// Многоточие обрезанного текста.
const ELLIPSIS: char = '…';

/// Домен и заголовок страницы-источника.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PageLabel {
    title: String,
    domain: String,
}

impl PageLabel {
    /// Подпись из заголовка (может быть пустым) и домена.
    pub(crate) fn new(title: impl Into<String>, domain: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            domain: domain.into(),
        }
    }

    /// Заголовок страницы (пустой — у страницы без `<title>`).
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    /// Домен страницы.
    pub(crate) fn domain(&self) -> &str {
        &self.domain
    }

    /// Заголовок окна: «<заголовок> — <домен>»; без заголовка — «<домен>».
    ///
    /// Суффикса «— Fastiplayer» нет: заголовок окна и так узкий, а приложение окно
    /// опознаёт по значку и app-id рабочего стола; два тире подряд только съели бы место
    /// у полезной части. Домен не режется, пока он сам влезает: режется заголовок
    /// страницы.
    pub(crate) fn window_title(&self) -> String {
        let domain_chars = self.domain.chars().count();
        let title_budget = MAX_WINDOW_TITLE_CHARS
            .saturating_sub(domain_chars)
            .saturating_sub(TITLE_DOMAIN_SEPARATOR.chars().count());
        if self.title.trim().is_empty() || title_budget < MIN_USEFUL_TITLE_CHARS {
            return truncate_with_ellipsis(&self.domain, MAX_WINDOW_TITLE_CHARS);
        }
        format!(
            "{}{TITLE_DOMAIN_SEPARATOR}{}",
            truncate_with_ellipsis(self.title.trim(), title_budget),
            self.domain
        )
    }
}

/// Данные страницы в журнал не попадают, даже если кто-то напишет `?label`.
impl fmt::Debug for PageLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PageLabel(<hidden>)")
    }
}

/// Обрезает текст до `max_chars` СИМВОЛОВ (не байт: кириллица и эмодзи не ломаются),
/// последним символом ставит многоточие.
fn truncate_with_ellipsis(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let kept = max_chars.saturating_sub(1);
    let mut shortened: String = text.chars().take(kept).collect();
    shortened.push(ELLIPSIS);
    shortened
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_and_domain_are_joined_with_a_dash() {
        let label = PageLabel::new("Cats of the day", "video.example");
        assert_eq!(label.window_title(), "Cats of the day — video.example");
    }

    #[test]
    fn an_empty_or_blank_title_leaves_just_the_domain() {
        assert_eq!(
            PageLabel::new("", "video.example").window_title(),
            "video.example"
        );
        assert_eq!(
            PageLabel::new("  ", "video.example").window_title(),
            "video.example"
        );
    }

    #[test]
    fn a_long_title_is_cut_by_characters_and_the_domain_stays_whole() {
        // Кириллица — 2 байта на символ: обрезка по байтам разрезала бы символ.
        let label = PageLabel::new("Ж".repeat(300), "video.example");
        let shown = label.window_title();
        assert_eq!(shown.chars().count(), MAX_WINDOW_TITLE_CHARS);
        assert!(shown.ends_with(" — video.example"));
        assert!(shown.contains("Ж…"));
    }

    #[test]
    fn a_domain_that_leaves_no_room_for_the_title_is_shown_alone() {
        let domain = format!("{}.example", "d".repeat(82));
        let label = PageLabel::new("Some title", domain.clone());
        assert_eq!(label.window_title(), domain);
    }

    #[test]
    fn debug_output_hides_the_page() {
        let label = PageLabel::new("unique-title", "unique-domain.example");
        assert_eq!(format!("{label:?}"), "PageLabel(<hidden>)");
    }
}
