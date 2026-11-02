//! Робот «отдать вкладку» (решение 175 сессии 09a).
//!
//! Страница сайта не может приказать экстеншену «отдай меня плееру» — это внутренняя
//! команда, которую принимает только фон от страниц самого экстеншена (в продукте её шлёт
//! кнопка попапа). Робот стенда делает то же, что кнопка: открывает служебную страницу
//! экстеншена, находит в ней вкладку стенда через `tabs.query` и шлёт фону
//! `{kind:"hand_off_tab", tab_id}`. Ответ фона разбирается СТРОГО: незнакомый вид исхода
//! или лишнее поле — ошибка, а не молчаливое «успех».
//!
//! Здесь три вещи, не зависящие от браузера: текст скрипта, разбор ответа и тип исхода.
//! Как скрипт доставить в страницу (CDP или Marionette), решают `chromium` и `firefox`.

use serde::Deserialize;
use serde_json::Value;

/// Страница экстеншена, из которой робот шлёт команду. Страница настроек не лезет к
/// фону сама (в отличие от попапа, который сразу открывает порт статуса).
pub(crate) const HAND_OFF_EXTENSION_PAGE: &str = "options.html";

/// Исход команды `hand_off_tab` (зеркало `HandOffTabOutcome` из
/// `extensions/browser/src/shared/handoff-contract.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HandOffOutcome {
    /// Фон начал передачу выбранного видео.
    Started,
    /// Во вкладке нет видео с живым `MediaSource`.
    NoVideo,
    /// Связь с плеером не поднялась или плеер отказал.
    PlayerUnavailable {
        /// Текст причины для журнала.
        reason: String,
    },
    /// Внутренняя ошибка экстеншена.
    Failed {
        /// Текст причины для журнала.
        reason: String,
    },
}

/// Почему робот не получил исход от фона.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HandOffError {
    /// Вкладки стенда не нашлось среди вкладок браузера.
    #[error("no browser tab starts with {prefix}; open tabs: {open_urls:?}")]
    NoStandTab {
        /// Какой префикс адреса искали.
        prefix: String,
        /// Адреса вкладок, которые видел экстеншен.
        open_urls: Vec<String>,
    },
    /// Фон не ответил: обработчика команды нет (экстеншен без передачи) или он закрыл порт.
    #[error("the extension background did not answer hand_off_tab: {detail}")]
    NoReply {
        /// Что сказал браузер («Receiving end does not exist», пустой ответ и т. п.).
        detail: String,
    },
    /// Ответ фона не похож на исход договора.
    #[error("the hand_off_tab reply is not a valid outcome ({problem}): {raw}")]
    BadReply {
        /// Что именно не так.
        problem: String,
        /// Ответ как есть.
        raw: String,
    },
    /// Служебная страница ещё не получила API экстеншена (робот повторит попытку).
    #[error("the extension page has no extension API yet")]
    PageNotReady,
    /// Сам скрипт робота упал.
    #[error("the robot script failed: {0}")]
    ScriptFailed(String),
}

/// Скрипт робота как функция `(api, urlPrefix) => Promise<string>`: возвращает JSON-строку
/// вида `{"robot":"reply","reply":…}` | `{"robot":"no_tab","urls":[…]}` |
/// `{"robot":"no_reply","error":"…"}` | `{"robot":"not_ready"}`.
///
/// `api` — `chrome` или `browser`; скрипт не знает, какой браузер, и ничего не ждёт
/// сверх одного `sendMessage`.
pub(crate) const ROBOT_FUNCTION: &str = r#"async (api, urlPrefix) => {
  if (!api || !api.tabs || !api.runtime) return JSON.stringify({ robot: "not_ready" });
  const tabs = await api.tabs.query({});
  const target = tabs.find((tab) => typeof tab.url === "string" && tab.url.startsWith(urlPrefix));
  if (!target) return JSON.stringify({ robot: "no_tab", urls: tabs.map((tab) => String(tab.url)) });
  try {
    const reply = await api.runtime.sendMessage({ kind: "hand_off_tab", tab_id: target.id });
    if (reply === undefined || reply === null) {
      return JSON.stringify({ robot: "no_reply", error: "the background answered with nothing" });
    }
    return JSON.stringify({ robot: "reply", reply });
  } catch (error) {
    return JSON.stringify({ robot: "no_reply", error: String(error && error.message || error) });
  }
}"#;

/// Ответ скрипта робота до разбора исхода.
#[derive(Debug, Deserialize)]
#[serde(tag = "robot", rename_all = "snake_case")]
enum RobotReply {
    Reply { reply: Value },
    NoTab { urls: Vec<String> },
    NoReply { error: String },
    NotReady,
}

/// Разбирает строку, которую вернул скрипт робота, в исход команды.
pub(crate) fn parse_robot_output(
    raw: &str,
    url_prefix: &str,
) -> Result<HandOffOutcome, HandOffError> {
    let robot: RobotReply = serde_json::from_str(raw).map_err(|error| HandOffError::BadReply {
        problem: format!("the robot output is not JSON of the expected shape: {error}"),
        raw: raw.to_owned(),
    })?;
    match robot {
        RobotReply::NotReady => Err(HandOffError::PageNotReady),
        RobotReply::NoTab { urls } => Err(HandOffError::NoStandTab {
            prefix: url_prefix.to_owned(),
            open_urls: urls,
        }),
        RobotReply::NoReply { error } => Err(HandOffError::NoReply { detail: error }),
        RobotReply::Reply { reply } => {
            let text = reply.to_string();
            let outcome: HandOffOutcome =
                serde_json::from_value(reply.clone()).map_err(|error| HandOffError::BadReply {
                    problem: error.to_string(),
                    raw: text.clone(),
                })?;
            // serde не запрещает лишние поля у вариантов без данных: проверяем форму сами.
            let expected_keys = match outcome {
                HandOffOutcome::Started | HandOffOutcome::NoVideo => 1,
                HandOffOutcome::PlayerUnavailable { .. } | HandOffOutcome::Failed { .. } => 2,
            };
            if reply.as_object().map_or(0, serde_json::Map::len) != expected_keys {
                return Err(HandOffError::BadReply {
                    problem: "unexpected extra fields".to_owned(),
                    raw: text,
                });
            }
            Ok(outcome)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIX: &str = "http://127.0.0.1:1/site/pages/mse-fmp4.html";

    fn parse(raw: &str) -> Result<HandOffOutcome, HandOffError> {
        parse_robot_output(raw, PREFIX)
    }

    #[test]
    fn every_contract_outcome_is_recognised() {
        assert_eq!(
            parse(r#"{"robot":"reply","reply":{"kind":"started"}}"#),
            Ok(HandOffOutcome::Started)
        );
        assert_eq!(
            parse(r#"{"robot":"reply","reply":{"kind":"no_video"}}"#),
            Ok(HandOffOutcome::NoVideo)
        );
        assert_eq!(
            parse(r#"{"robot":"reply","reply":{"kind":"player_unavailable","reason":"no hello"}}"#),
            Ok(HandOffOutcome::PlayerUnavailable {
                reason: "no hello".into()
            })
        );
        assert_eq!(
            parse(r#"{"robot":"reply","reply":{"kind":"failed","reason":"boom"}}"#),
            Ok(HandOffOutcome::Failed {
                reason: "boom".into()
            })
        );
    }

    #[test]
    fn unknown_kind_missing_reason_and_extra_fields_are_refused() {
        for reply in [
            r#"{"kind":"started_maybe"}"#,
            r#"{"kind":"failed"}"#,
            r#"{"kind":"started","extra":1}"#,
            r#"{"kind":"failed","reason":7}"#,
            r#"{}"#,
            r#""started""#,
        ] {
            let raw = format!(r#"{{"robot":"reply","reply":{reply}}}"#);
            assert!(
                matches!(parse(&raw), Err(HandOffError::BadReply { .. })),
                "{reply} must be refused"
            );
        }
    }

    #[test]
    fn robot_level_problems_keep_their_own_error() {
        assert_eq!(
            parse(r#"{"robot":"not_ready"}"#),
            Err(HandOffError::PageNotReady)
        );
        assert_eq!(
            parse(r#"{"robot":"no_tab","urls":["about:blank"]}"#),
            Err(HandOffError::NoStandTab {
                prefix: PREFIX.into(),
                open_urls: vec!["about:blank".into()]
            })
        );
        assert_eq!(
            parse(r#"{"robot":"no_reply","error":"Receiving end does not exist"}"#),
            Err(HandOffError::NoReply {
                detail: "Receiving end does not exist".into()
            })
        );
        assert!(matches!(
            parse("garbage"),
            Err(HandOffError::BadReply { .. })
        ));
    }
}
