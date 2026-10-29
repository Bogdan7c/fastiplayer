//! Правило идемпотентности `open_session` (инвариант 6, решение 40).
//!
//! Таблицу сеансов держит плеер (сессия 02); здесь только решение «что делать
//! с запросом», одно на все реализации. Браузерный режим ведёт один сеанс за
//! раз: новое видео из другой вкладки заменяет текущее, а повтор того же
//! запроса (экстеншен перезапустил service worker, человек нажал кнопку
//! дважды) не открывает второго окна.

use crate::control::GrantOutcome;
use crate::text::SessionKey;

/// Что сделать с запросом `open_session`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenSessionDecision {
    /// Сеансов нет — открыть новый.
    StartNew,
    /// Идёт сеанс с этим же ключом — выдать пропуск в него, ничего не открывая.
    ReuseActive,
    /// Идёт сеанс другой вкладки — закрыть его (`replaced_by_another_tab`) и
    /// открыть новый.
    ReplaceActive,
}

impl OpenSessionDecision {
    /// Как сообщить итог экстеншену в `session_granted`.
    #[must_use]
    pub const fn grant_outcome(self) -> GrantOutcome {
        match self {
            Self::StartNew => GrantOutcome::Started,
            Self::ReuseActive => GrantOutcome::ReusedExisting,
            Self::ReplaceActive => GrantOutcome::ReplacedPrevious,
        }
    }
}

/// Решает, что делать с запросом `open_session`.
///
/// `active_session` — ключ сеанса, который сейчас идёт в браузерном режиме.
#[must_use]
pub fn decide_open_session(
    active_session: Option<&SessionKey>,
    requested: &SessionKey,
) -> OpenSessionDecision {
    match active_session {
        None => OpenSessionDecision::StartNew,
        Some(active) if active == requested => OpenSessionDecision::ReuseActive,
        Some(_) => OpenSessionDecision::ReplaceActive,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> SessionKey {
        match SessionKey::new(text) {
            Ok(key) => key,
            Err(violation) => panic!("тестовый ключ {text:?} не прошёл проверку: {violation}"),
        }
    }

    #[test]
    fn repeated_request_with_the_same_key_reuses_the_session_every_time() {
        let active = key("tab-7-video-1");
        // Сколько бы раз ни повторили — каждый раз тот же сеанс, а не новое окно.
        for _ in 0..3 {
            let decision = decide_open_session(Some(&active), &key("tab-7-video-1"));
            assert_eq!(decision, OpenSessionDecision::ReuseActive);
            assert_eq!(decision.grant_outcome(), GrantOutcome::ReusedExisting);
        }
    }

    #[test]
    fn other_tab_replaces_and_empty_player_starts_new() {
        let active = key("tab-7-video-1");
        let replace = decide_open_session(Some(&active), &key("tab-9-video-1"));
        assert_eq!(replace, OpenSessionDecision::ReplaceActive);
        assert_eq!(replace.grant_outcome(), GrantOutcome::ReplacedPrevious);
        let start = decide_open_session(None, &active);
        assert_eq!(start, OpenSessionDecision::StartNew);
        assert_eq!(start.grant_outcome(), GrantOutcome::Started);
    }
}
