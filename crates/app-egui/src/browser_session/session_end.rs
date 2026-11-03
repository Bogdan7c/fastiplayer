//! Конец сеанса глазами зрителя окна (мост, сессия 04b, решение 42).
//!
//! Вкладка ушла (или экстеншен пропал и не вернулся) — процесс доигрывает скачанное
//! (03c). Окно должно сказать человеку, что происходит:
//! - пока доигрывание играет — ничего (смотрим видео);
//! - окно на паузе ждёт человека — постоянное сообщение «вкладка закрыта, скачанное
//!   можно досмотреть» ([`SessionEndView::WaitingForViewer`]);
//! - доигрывание кончилось — короткое сообщение по причине и итогу
//!   ([`SessionEndView::Finished`]), после которого окно закрывается само.
//!
//! Концы без доигрывания, о которых окно всё же говорит (тоже короткое сообщение и
//! закрытие): обрыв по ошибке потока со стороны плеера ([`SessionEndView::StreamError`],
//! решение 132) и честные концы со стороны страницы (сессия 09a2) — страница забрала
//! видео ([`SessionEndView::PageTookVideoBack`], код 4009, решение 179) и сбой на стороне
//! страницы ([`SessionEndView::PageStreamError`], её код 4003).
//!
//! Здесь только чистые правила «что показать» и стабильные метки журнала; когда
//! показывать и сколько держать — забота окна (`window::end_notice`), а состояние
//! доигрывания — медиа-части (`session_media::ending`). Тексты — только английские
//! (пользовательский текст UI), без домена и заголовка страницы (инвариант 7).

use browser_bridge_protocol::SessionCloseReason;
use browser_bridge_session::SessionEndReason;

use super::session_media::DrainEnd;

/// Почему сеанс доигрывается — из причины конца, которая ведёт к доигрыванию.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DrainCause {
    /// Страница сказала `tab_gone`: вкладку закрыли, ушли со страницы, закрыли браузер.
    TabClosed,
    /// Экстеншен пропал без `tab_gone` и не вернулся за срок возврата (браузер упал).
    BrowserConnectionLost,
}

impl DrainCause {
    /// Причина конца → причина доигрывания; `None` — этот конец не доигрывается.
    ///
    /// Совпадает с единым правилом `browser_bridge_session::ends_with_playback_drain`
    /// (закреплено тестом): сообщение показывается ровно тогда, когда процесс доигрывает.
    pub(crate) const fn of(reason: SessionEndReason) -> Option<Self> {
        match reason {
            SessionEndReason::ClosedByPlayer(SessionCloseReason::TabGone) => Some(Self::TabClosed),
            SessionEndReason::ReconnectGraceExpired => Some(Self::BrowserConnectionLost),
            SessionEndReason::ClosedByPlayer(_)
            | SessionEndReason::ClosedByPage { .. }
            | SessionEndReason::FirstAttachDeadlineExpired => None,
        }
    }

    /// Стабильная метка для журнала.
    pub(crate) const fn log_label(self) -> &'static str {
        match self {
            Self::TabClosed => "tab_gone",
            Self::BrowserConnectionLost => "reconnect_grace_expired",
        }
    }
}

/// Доигрывание кончилось: почему доигрывали и чем кончилось.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DrainFinish {
    /// Почему доигрывали.
    pub(crate) cause: DrainCause,
    /// Чем кончилось.
    pub(crate) end: DrainEnd,
}

/// Что окно должно сказать о конце сеанса прямо сейчас.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionEndView {
    /// Сказать нечего: сеанс идёт, доигрывание играет или конец не доигрывается.
    Silent,
    /// Окно на паузе ждёт человека; скачанное можно досмотреть.
    WaitingForViewer(DrainCause),
    /// Доигрывание кончилось: сообщение и закрытие окна.
    Finished(DrainFinish),
    /// Связь оборвана по ошибке потока (код 4003, решение 132): видео возвращено странице
    /// (если плеер сам рвал связь — с прощанием `return_to_page`), доигрывать нечего.
    /// Сообщение держится так же, как у конца доигрывания, потом окно закрывается.
    /// Не через [`DrainCause`]: правило «сообщение ⇔ доигрывание» остаётся нетронутым.
    ///
    /// Это сбой, замеченный ПЛЕЕРОМ (он сам закрыл связь кодом 4003). Сбой, замеченный
    /// страницей, — [`Self::PageStreamError`]: текст тот же, причина в журнале другая.
    StreamError,
    /// Страница закрыла связь кодом 4003 (сессия 09a2, Д-6): сбой на стороне страницы
    /// (поток битый, тень не встала, кусок не отправился), экстеншен сам снял тень и вернул
    /// видео странице. Зритель видит то же «Stream error — video returned to the page»;
    /// прощания `return_to_page` нет и быть не может — сокет уже закрыт страницей.
    PageStreamError,
    /// Страница честно забрала видео, вкладка на месте (код 4009, решение 179): короткое
    /// «The page took the video back» и закрытие окна. Без доигрывания (страница уже играет
    /// сама — было бы два звука) и без `return_to_page` (видео и так у страницы).
    PageTookVideoBack,
}

impl SessionEndView {
    /// Представление конца сеанса, который не доигрывается, но не молчит; `None` — конец
    /// молчаливый.
    ///
    /// Говорящие концы: обрыв по ошибке потока со стороны плеера (его 4003) и честные концы
    /// страницы, названные нашим кодом протокола ([`Self::of_page_named_reason`]).
    pub(crate) fn of_undrained_end(reason: SessionEndReason) -> Option<Self> {
        if reason == SessionEndReason::ClosedByPlayer(SessionCloseReason::ProtocolViolation) {
            return Some(Self::StreamError);
        }
        reason
            .reason_named_by_page()
            .and_then(Self::of_page_named_reason)
    }

    /// Что сказать о конце, который страница назвала кодом протокола.
    ///
    /// Говорят только два кода, у которых для зрителя есть смысл: 4009 (страница забрала
    /// видео) и 4003 (сбой на стороне страницы). Остальные закрытия страницей молчат, как
    /// раньше:
    /// - обычное 1000 и кадр без кода (до этого метода не доходят — `None` у
    ///   `reason_named_by_page`): экстеншен так завершает передачу, конец которой уже
    ///   решён и объяснён в другом месте (ответ на наш `end_handoff`, неудачный старт до
    ///   показа видео). Сбои страницы с 09a2 приходят кодом 4003, а не 1000;
    /// - прочие наши коды (4000, 4001, 4002…) — это слова ПЛЕЕРА; страница ими связь не
    ///   закрывает, а `tab_gone` приходит сообщением и закрывается плеером. Выдумывать
    ///   для них текст — значит гадать.
    const fn of_page_named_reason(named: SessionCloseReason) -> Option<Self> {
        match named {
            SessionCloseReason::PageTookVideoBack => Some(Self::PageTookVideoBack),
            SessionCloseReason::ProtocolViolation => Some(Self::PageStreamError),
            SessionCloseReason::PlayerClosed
            | SessionCloseReason::ReplacedByAnotherTab
            | SessionCloseReason::TabGone
            | SessionCloseReason::InvalidToken
            | SessionCloseReason::UnknownSession
            | SessionCloseReason::PlayerShuttingDown
            | SessionCloseReason::AttachTimeout
            | SessionCloseReason::AlreadyAttached => None,
        }
    }
}

/// Тексты. Короткие: окно маленькое, а читать их некогда — через пару секунд оно закроется.
const TAB_CLOSED: &str = "Source tab closed";
const TAB_CLOSED_STALLED: &str = "Source tab closed — playback stalled";
const TAB_CLOSED_FAILED: &str = "Source tab closed — playback failed";
const TAB_CLOSED_WAITING: &str = "Source tab closed — you can still watch what was downloaded";
const STREAM_ERROR: &str = "Stream error — video returned to the page";
const PAGE_TOOK_VIDEO_BACK: &str = "The page took the video back";
const BROWSER_LOST: &str = "Lost connection to the browser";
const BROWSER_LOST_STALLED: &str = "Lost connection to the browser — playback stalled";
const BROWSER_LOST_FAILED: &str = "Lost connection to the browser — playback failed";
const BROWSER_LOST_WAITING: &str =
    "Lost connection to the browser — you can still watch what was downloaded";

impl SessionEndView {
    /// Текст сообщения в центре окна (`None` — сообщения нет).
    pub(crate) const fn center_message(self) -> Option<&'static str> {
        match self {
            Self::Silent => None,
            Self::WaitingForViewer(DrainCause::TabClosed) => Some(TAB_CLOSED_WAITING),
            Self::WaitingForViewer(DrainCause::BrowserConnectionLost) => Some(BROWSER_LOST_WAITING),
            Self::Finished(finish) => Some(finished_message(finish)),
            // Сбой с любой стороны зритель видит одинаково: видео снова у страницы.
            Self::StreamError | Self::PageStreamError => Some(STREAM_ERROR),
            Self::PageTookVideoBack => Some(PAGE_TOOK_VIDEO_BACK),
        }
    }

    /// Стабильные метки для журнала: вид сообщения, причина, итог (`none` — нет).
    pub(crate) const fn log_labels(self) -> SessionEndLogLabels {
        match self {
            Self::Silent => SessionEndLogLabels {
                notice: "silent",
                cause: "none",
                end: "none",
            },
            Self::WaitingForViewer(cause) => SessionEndLogLabels {
                notice: "waiting_for_viewer",
                cause: cause.log_label(),
                end: "none",
            },
            Self::Finished(finish) => SessionEndLogLabels {
                notice: "finished",
                cause: finish.cause.log_label(),
                end: finish.end.log_label(),
            },
            // Метки ниже — договор со стендом моста (сессия 09a2): не переименовывать.
            Self::StreamError => SessionEndLogLabels {
                notice: "stream_error",
                cause: "protocol_violation",
                end: "none",
            },
            Self::PageStreamError => SessionEndLogLabels {
                notice: "stream_error",
                cause: "page_stream_error",
                end: "none",
            },
            Self::PageTookVideoBack => SessionEndLogLabels {
                notice: "page_took_video_back",
                cause: "page_took_video_back",
                end: "none",
            },
        }
    }
}

/// Метки журнала одного сообщения (без данных страницы).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionEndLogLabels {
    /// `silent` / `waiting_for_viewer` / `finished` / `stream_error` /
    /// `page_took_video_back`.
    pub(crate) notice: &'static str,
    /// Метка [`DrainCause`], сторона сбоя потока (`protocol_violation` — плеер,
    /// `page_stream_error` — страница), `page_took_video_back` или `none`.
    pub(crate) cause: &'static str,
    /// Метка [`DrainEnd`] или `none`.
    pub(crate) end: &'static str,
}

/// Текст конца доигрывания: причина + короткий хвост, если видео не доиграло честно.
///
/// «Нечего играть», «доиграно» и пауза (последняя — только у безоконного процесса,
/// где окна нет) звучат одинаково: зрителю важно, что вкладки больше нет.
const fn finished_message(finish: DrainFinish) -> &'static str {
    match (finish.cause, finish.end) {
        (DrainCause::TabClosed, DrainEnd::Stalled) => TAB_CLOSED_STALLED,
        (DrainCause::TabClosed, DrainEnd::PlayerFault) => TAB_CLOSED_FAILED,
        (
            DrainCause::TabClosed,
            DrainEnd::PlayedToEnd | DrainEnd::NothingToPlay | DrainEnd::PagePaused,
        ) => TAB_CLOSED,
        (DrainCause::BrowserConnectionLost, DrainEnd::Stalled) => BROWSER_LOST_STALLED,
        (DrainCause::BrowserConnectionLost, DrainEnd::PlayerFault) => BROWSER_LOST_FAILED,
        (
            DrainCause::BrowserConnectionLost,
            DrainEnd::PlayedToEnd | DrainEnd::NothingToPlay | DrainEnd::PagePaused,
        ) => BROWSER_LOST,
    }
}

#[cfg(test)]
mod tests {
    use browser_bridge_session::ends_with_playback_drain;

    use super::*;

    /// Все причины конца, которые бывают (для сверки с правилом сервера).
    fn every_end_reason() -> Vec<SessionEndReason> {
        let mut reasons: Vec<SessionEndReason> = [
            SessionCloseReason::PlayerClosed,
            SessionCloseReason::ReplacedByAnotherTab,
            SessionCloseReason::TabGone,
            SessionCloseReason::ProtocolViolation,
            SessionCloseReason::InvalidToken,
            SessionCloseReason::UnknownSession,
            SessionCloseReason::PlayerShuttingDown,
            SessionCloseReason::AttachTimeout,
            SessionCloseReason::AlreadyAttached,
            SessionCloseReason::PageTookVideoBack,
        ]
        .into_iter()
        .map(SessionEndReason::ClosedByPlayer)
        .collect();
        reasons.extend([
            SessionEndReason::ClosedByPage { code: None },
            SessionEndReason::ClosedByPage { code: Some(1000) },
            SessionEndReason::ClosedByPage {
                code: Some(SessionCloseReason::PageTookVideoBack.close_code()),
            },
            SessionEndReason::ClosedByPage {
                code: Some(SessionCloseReason::ProtocolViolation.close_code()),
            },
            SessionEndReason::ClosedByPage {
                code: Some(SessionCloseReason::PlayerClosed.close_code()),
            },
            // Чужой прикладной код: не наш, ничего не значит.
            SessionEndReason::ClosedByPage { code: Some(4999) },
            SessionEndReason::ReconnectGraceExpired,
            SessionEndReason::FirstAttachDeadlineExpired,
        ]);
        reasons
    }

    #[test]
    fn a_message_is_due_exactly_when_the_server_rule_drains_the_playback() {
        for reason in every_end_reason() {
            assert_eq!(
                DrainCause::of(reason).is_some(),
                ends_with_playback_drain(reason),
                "{reason:?}"
            );
        }
    }

    #[test]
    fn the_tab_and_the_lost_browser_get_their_own_words() {
        let finished = |cause, end| SessionEndView::Finished(DrainFinish { cause, end });
        assert_eq!(
            finished(DrainCause::TabClosed, DrainEnd::PlayedToEnd).center_message(),
            Some("Source tab closed")
        );
        assert_eq!(
            finished(DrainCause::TabClosed, DrainEnd::NothingToPlay).center_message(),
            Some("Source tab closed")
        );
        assert_eq!(
            finished(DrainCause::TabClosed, DrainEnd::Stalled).center_message(),
            Some("Source tab closed — playback stalled")
        );
        assert_eq!(
            finished(DrainCause::TabClosed, DrainEnd::PlayerFault).center_message(),
            Some("Source tab closed — playback failed")
        );
        assert_eq!(
            finished(DrainCause::BrowserConnectionLost, DrainEnd::PlayedToEnd).center_message(),
            Some("Lost connection to the browser")
        );
        assert_eq!(
            finished(DrainCause::BrowserConnectionLost, DrainEnd::Stalled).center_message(),
            Some("Lost connection to the browser — playback stalled")
        );
        assert_eq!(
            SessionEndView::WaitingForViewer(DrainCause::TabClosed).center_message(),
            Some("Source tab closed — you can still watch what was downloaded")
        );
        assert_eq!(
            SessionEndView::WaitingForViewer(DrainCause::BrowserConnectionLost).center_message(),
            Some("Lost connection to the browser — you can still watch what was downloaded")
        );
        assert_eq!(SessionEndView::Silent.center_message(), None);
    }

    /// Какой вид у конца без доигрывания должен быть (ожидание теста, выписанное руками).
    fn expected_undrained_view(reason: SessionEndReason) -> Option<SessionEndView> {
        let page_took_video_back = SessionEndReason::ClosedByPage {
            code: Some(SessionCloseReason::PageTookVideoBack.close_code()),
        };
        let page_stream_error = SessionEndReason::ClosedByPage {
            code: Some(SessionCloseReason::ProtocolViolation.close_code()),
        };
        if reason == SessionEndReason::ClosedByPlayer(SessionCloseReason::ProtocolViolation) {
            Some(SessionEndView::StreamError)
        } else if reason == page_took_video_back {
            Some(SessionEndView::PageTookVideoBack)
        } else if reason == page_stream_error {
            Some(SessionEndView::PageStreamError)
        } else {
            None
        }
    }

    #[test]
    fn only_stream_errors_and_the_page_taking_the_video_back_speak_and_they_never_drain() {
        for reason in every_end_reason() {
            let view = SessionEndView::of_undrained_end(reason);
            assert_eq!(view, expected_undrained_view(reason), "{reason:?}");
            // Сообщение об ошибке и сообщение доигрывания не пересекаются.
            assert!(
                !(view.is_some() && DrainCause::of(reason).is_some()),
                "{reason:?}"
            );
        }
        assert_eq!(
            SessionEndView::StreamError.center_message(),
            Some("Stream error — video returned to the page")
        );
        assert_eq!(
            SessionEndView::StreamError.log_labels(),
            SessionEndLogLabels {
                notice: "stream_error",
                cause: "protocol_violation",
                end: "none",
            }
        );
    }

    #[test]
    fn the_page_taking_the_video_back_is_told_briefly_and_is_never_played_on() {
        let reason = SessionEndReason::ClosedByPage {
            code: Some(SessionCloseReason::PageTookVideoBack.close_code()),
        };
        // Правило сервера: страница уже играет сама — доигрывания нет (решение 179).
        assert!(!ends_with_playback_drain(reason));
        assert_eq!(DrainCause::of(reason), None);
        let view = SessionEndView::of_undrained_end(reason).expect("the window speaks");
        assert_eq!(view.center_message(), Some("The page took the video back"));
        // Метки — договор со стендом (пакет E сессии 09a2).
        assert_eq!(
            view.log_labels(),
            SessionEndLogLabels {
                notice: "page_took_video_back",
                cause: "page_took_video_back",
                end: "none",
            }
        );
    }

    #[test]
    fn a_stream_error_on_the_page_side_reads_like_ours_but_logs_its_side() {
        let reason = SessionEndReason::ClosedByPage {
            code: Some(SessionCloseReason::ProtocolViolation.close_code()),
        };
        assert!(!ends_with_playback_drain(reason));
        let view = SessionEndView::of_undrained_end(reason).expect("the window speaks");
        assert_eq!(view, SessionEndView::PageStreamError);
        // Зритель видит тот же текст, что у сбоя со стороны плеера.
        assert_eq!(
            view.center_message(),
            SessionEndView::StreamError.center_message()
        );
        assert_eq!(
            view.log_labels(),
            SessionEndLogLabels {
                notice: "stream_error",
                cause: "page_stream_error",
                end: "none",
            }
        );
    }

    #[test]
    fn log_labels_are_stable_and_carry_no_page_data() {
        let finish = SessionEndView::Finished(DrainFinish {
            cause: DrainCause::TabClosed,
            end: DrainEnd::PlayedToEnd,
        });
        assert_eq!(
            finish.log_labels(),
            SessionEndLogLabels {
                notice: "finished",
                cause: "tab_gone",
                end: "played_to_end",
            }
        );
        assert_eq!(
            SessionEndView::WaitingForViewer(DrainCause::BrowserConnectionLost).log_labels(),
            SessionEndLogLabels {
                notice: "waiting_for_viewer",
                cause: "reconnect_grace_expired",
                end: "none",
            }
        );
    }
}
