//! Чтение попапа экстеншена глазами робота: какой случай он показывает (`data-notice`),
//! какие строки и кнопки в нём есть и на чём стоит фокус.
//!
//! Контракт разметки попапа (его задаёт экстеншен, робот только читает):
//! - `section.notice[data-notice="<id>"]` — сообщение (`in_player`, `no_video`, `protected` …);
//! - `button.video-row` — выбираемая строка, `div.video-row-static` — невыбираемая;
//! - `button.consent-allow` / `button.consent-deny` — вопрос про локальный адрес;
//! - `button.reload-and-open` — «Reload and open» при `data-notice="reload_needed"` (10d).
//!
//! Одна и та же функция-зонд исполняется в странице попапа обоих браузеров: у Chromium — через
//! CDP в цели попапа, у Firefox — кадровым скриптом внутри процесса попапа (chrome-контекст
//! Marionette сам до содержимого расширения не дотягивается).

use serde::Deserialize;

/// Функция-зонд: принимает `document` попапа и возвращает JSON-строку снимка.
pub(super) const PROBE_FUNCTION: &str = r#"(document) => {
  const text = (element) => (element.textContent || "").replace(/\s+/g, " ").trim();
  const notice = document.querySelector("section.notice");
  const focus = document.activeElement;
  const focused = focus && focus !== document.body
    ? { tag: focus.tagName.toLowerCase(), class: focus.className || "", text: text(focus) }
    : null;
  return JSON.stringify({
    notice: notice ? notice.getAttribute("data-notice") : null,
    selectable_rows: [...document.querySelectorAll("button.video-row")].map(text),
    static_rows: [...document.querySelectorAll("div.video-row-static")].map(text),
    has_consent_question: document.querySelector("button.consent-allow") !== null,
    has_reload_button: document.querySelector("button.reload-and-open") !== null,
    focused,
  });
}"#;

/// Что находится в фокусе попапа.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FocusedElement {
    /// Имя тега строчными буквами.
    pub tag: String,
    /// Атрибут `class` целиком.
    pub class: String,
    /// Видимый текст элемента.
    pub text: String,
}

/// Снимок попапа.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PopupSnapshot {
    /// Идентификатор случая из `data-notice`; `None` — сообщения ещё нет (попап рисуется).
    pub notice: Option<String>,
    /// Тексты выбираемых строк, сверху вниз (порядок DOM = порядок Tab).
    pub selectable_rows: Vec<String>,
    /// Тексты невыбираемых строк.
    pub static_rows: Vec<String>,
    /// Есть ли вопрос про локальный адрес (кнопки Allow / Don't allow).
    pub has_consent_question: bool,
    /// Есть ли кнопка «Reload and open» (вкладка открыта раньше нашего скрипта, 10d).
    pub has_reload_button: bool,
    /// Элемент в фокусе; `None` — фокус на самой странице.
    pub focused: Option<FocusedElement>,
}

impl PopupSnapshot {
    /// Разбирает ответ зонда.
    pub(super) fn parse(raw: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(raw)
    }

    /// Стоит ли фокус на кнопке с классом `class` (например, `consent-allow`).
    pub fn focus_is_button_with_class(&self, class: &str) -> bool {
        self.focused.as_ref().is_some_and(|element| {
            element.tag == "button" && element.class.split_whitespace().any(|name| name == class)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_probe_answer_and_reads_the_focus() {
        let raw = r#"{"notice":"in_player","selectable_rows":["a 160×90","b"],"static_rows":[],
            "has_consent_question":true,"has_reload_button":false,
            "focused":{"tag":"button","class":"consent-allow","text":"Allow"}}"#;
        let snapshot = PopupSnapshot::parse(raw).expect("valid answer");
        assert_eq!(snapshot.notice.as_deref(), Some("in_player"));
        assert_eq!(snapshot.selectable_rows.len(), 2);
        assert!(snapshot.focus_is_button_with_class("consent-allow"));
        assert!(!snapshot.focus_is_button_with_class("consent-deny"));
    }

    #[test]
    fn a_popup_without_focus_or_notice_is_still_a_valid_snapshot() {
        let raw = r#"{"notice":null,"selectable_rows":[],"static_rows":[],"has_consent_question":false,"has_reload_button":false,"focused":null}"#;
        let snapshot = PopupSnapshot::parse(raw).expect("valid answer");
        assert_eq!(snapshot.notice, None);
        assert!(!snapshot.focus_is_button_with_class("consent-allow"));
    }

    #[test]
    fn garbage_is_an_error_not_an_empty_snapshot() {
        assert!(PopupSnapshot::parse("not json").is_err());
    }
}
