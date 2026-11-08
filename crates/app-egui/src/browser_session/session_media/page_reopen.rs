//! Возврат страницы — «магия возврата вкладки» (решение 130) на стороне плеера:
//! протокол 1.4, «сирота», сессия 11b1, решения 240–241.
//!
//! ```text
//!  page_left ──► досмотр (как после tab_gone) + Ready ── часы странице идут (пульс сироты)
//!  зритель: play ──► Ready → reopen_page{T} → Requested{срок}
//!  page_returned ──► досмотр отменён, тот же сеанс, новое поколение (03e)
//!  page_reopen_failed / связь закрылась / срок вышел ──► Refused: запасной путь 04b
//! ```
//!
//! Чем владеет модуль (и больше ничем):
//! - стадией возврата [`PageReopenStage`]: можно ли просить, ждём ли ответа, чем кончилось;
//! - правилом «одна просьба на сироту»: отказ окончательный (решение 130 — «если нет,
//!   сообщение и закрытие»), повторный play просьбу не повторяет;
//! - правилом «пока просьба в пути, окно не закрывается»: конец скачанного не кончает
//!   досмотр, окно говорит «Reopening the page…» (`SessionEndView::ReopeningPage`);
//! - своим сроком ожидания ([`PageReopenPolicy::patience`]): экстеншен молчит — отказ;
//! - сообщением окну за шаг ([`PageReopenNotice`]): «открываем» и «не вышло, почему».
//!
//! Досмотр, его правила и тексты конца — по-прежнему [`super::ending`] и [`super::drain`];
//! что слать странице — [`super::super::outbound`]; когда выходить процессу — машина
//! выхода (`ProcessLifecycle::on_page_left` / `on_page_returned`, их зовёт главный цикл
//! по тем же уведомлениям).

use std::time::{Duration, Instant};

use browser_bridge_protocol::session::{PageReopenFailure, TabGoneReason};
use browser_bridge_session::{SessionEndReason, SessionRef};
use tracing::{debug, info, warn};

use super::SessionMedia;
use super::drain::DrainProgress;
use super::farewell::FarewellPosition;
use crate::browser_session::intake_notice::PageLinkChange;
use crate::browser_session::outbound::ReopenRequestOutcome;

/// Сроки возврата страницы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageReopenPolicy {
    /// Сколько ждать `page_returned` или отказа после просьбы. Экстеншен ждёт загрузку
    /// страницы и её видео своим сроком (сессия 11b2) и отвечает раньше; этот срок —
    /// страховка от молчания, чтобы окно не висело со спиннером вечно.
    pub(crate) patience: Duration,
}

impl PageReopenPolicy {
    /// 45 с: страница с рекламой и медленной сетью грузится десятки секунд, а дольше
    /// человек у застывшего окна не ждёт. Это поведение протокола, а не настройка.
    pub(crate) const fn recommended() -> Self {
        Self {
            patience: Duration::from_secs(45),
        }
    }
}

/// Почему страницу не вернули.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageReopenRefusal {
    /// Экстеншен попробовал и сказал почему (`page_reopen_failed`).
    Extension(PageReopenFailure),
    /// Связь с экстеншеном закрылась или пропала (браузер закрыт, экстеншен сдался).
    LinkLost,
    /// Экстеншен не ответил за [`PageReopenPolicy::patience`].
    NoAnswer,
    /// Просьбу не на чем было отправить: позиции нет или версия связи не та.
    NotSent,
}

impl PageReopenRefusal {
    /// Стабильная метка для журнала (её читают сквозные тесты).
    pub(crate) const fn log_label(self) -> &'static str {
        match self {
            Self::Extension(PageReopenFailure::NoBrowserWindow) => "no_browser_window",
            Self::Extension(PageReopenFailure::PageDidNotLoad) => "page_did_not_load",
            Self::Extension(PageReopenFailure::NotTheSameVideo) => "not_the_same_video",
            Self::Extension(PageReopenFailure::VideoDidNotStart) => "video_did_not_start",
            Self::LinkLost => "link_lost",
            Self::NoAnswer => "no_answer",
            Self::NotSent => "not_sent",
        }
    }
}

/// Стадия возврата страницы текущего сеанса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageReopenStage {
    /// Возвращать нечего: страница на месте, ушла без связи (`tab_gone`) или сеанса нет.
    Unavailable,
    /// Страница ушла, связь держится: просьбу можно послать.
    Ready,
    /// Просьба поставлена в отправку; ждём ответа до срока.
    Requested {
        /// Когда ждать перестаём.
        deadline: Instant,
    },
    /// Вернуть не вышло; отказ окончательный.
    Refused(PageReopenRefusal),
}

/// Что окно должно сказать зрителю о возврате страницы за этот шаг.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PageReopenNotice {
    /// Говорить нечего.
    #[default]
    Nothing,
    /// Просьба ушла: «открываем страницу».
    Reopening,
    /// Вернуть не вышло.
    Refused(PageReopenRefusal),
}

/// Состояние возврата страницы (поле [`SessionMedia`]).
#[derive(Debug, Clone, Copy)]
pub(super) struct PageReopen {
    stage: PageReopenStage,
    notice: PageReopenNotice,
    policy: PageReopenPolicy,
}

impl Default for PageReopen {
    fn default() -> Self {
        Self {
            stage: PageReopenStage::Unavailable,
            notice: PageReopenNotice::Nothing,
            policy: PageReopenPolicy::recommended(),
        }
    }
}

impl PageReopen {
    /// Слышит ли нас экстеншен страницы, которой нет: часы (пульс) и просьба уходят ему.
    pub(super) const fn link_is_open(&self) -> bool {
        matches!(
            self.stage,
            PageReopenStage::Ready | PageReopenStage::Requested { .. }
        )
    }

    /// Ждём ли ответа на просьбу (тогда конец скачанного досмотр не кончает).
    pub(super) const fn awaits_answer(&self) -> bool {
        matches!(self.stage, PageReopenStage::Requested { .. })
    }

    /// Когда кончается ожидание ответа (`None` — не ждём).
    pub(super) const fn deadline(&self) -> Option<Instant> {
        match self.stage {
            PageReopenStage::Requested { deadline } => Some(deadline),
            _ => None,
        }
    }

    /// Отказ: стадия становится окончательной, окну — сообщение. Отказ без просьбы
    /// (связь закрылась, пока просить было не нужно) окну не говорится: зритель ничего
    /// не просил, досмотр и его тексты идут как в 04b.
    fn refuse(&mut self, refusal: PageReopenRefusal) {
        if self.awaits_answer() {
            self.notice = PageReopenNotice::Refused(refusal);
        }
        info!(class = %refusal.log_label(), "The page could not be brought back");
        self.stage = PageReopenStage::Refused(refusal);
    }

    /// Просьбу, которую зритель только что сделал play, не удалось даже отправить: отказ
    /// окончательный, и зритель о нём узнаёт (иначе его play «ничего не сделал» молча).
    fn refuse_unsent(&mut self) {
        self.refuse(PageReopenRefusal::NotSent);
        self.notice = PageReopenNotice::Refused(PageReopenRefusal::NotSent);
    }
}

impl SessionMedia {
    /// Страница сеанса ушла, вернулась или не вернулась (уведомление потока приёма).
    pub(super) fn follow_page_link(&mut self, change: PageLinkChange) {
        match change {
            PageLinkChange::Left(reason) => self.follow_page_left(reason),
            PageLinkChange::Returned => self.follow_page_returned(),
            PageLinkChange::ReopenFailed(failure) => {
                self.page_reopen
                    .refuse(PageReopenRefusal::Extension(failure));
            }
        }
    }

    /// Страница ушла, связь держится: досмотр (как после `tab_gone` с той же причиной), но
    /// страницу можно вернуть. Поток приёма уже запечатал вход: уведомление идёт после всех
    /// данных, принятых до ухода.
    fn follow_page_left(&mut self, reason: TabGoneReason) {
        if self.is_draining() {
            debug!("page_left while the playback already drains; ignored");
            return;
        }
        self.begin_drain(SessionEndReason::TabGone { reason });
        self.sealed_instance = self.current_session.clone();
        self.page_reopen.stage = PageReopenStage::Ready;
    }

    /// Страница вернулась в этот же сеанс (решение 241): досмотра больше нет, страница снова
    /// получает всё (часы, буфер, просьбы), её новое поколение придёт следом.
    fn follow_page_returned(&mut self) {
        info!("The page came back; the session continues");
        self.drain = None;
        self.sealed_instance = None;
        self.page_reopen.stage = PageReopenStage::Unavailable;
    }

    /// Зритель нажал play, пока страницы нет (зовёт транспорт зрителя). Просьба уходит
    /// один раз на сироту; без сироты (старый экстеншен, `tab_gone`) — ничего, как в 04b.
    pub(super) fn ask_page_to_come_back(&mut self, now: Instant) {
        if self.page_reopen.stage != PageReopenStage::Ready {
            return;
        }
        // T — та же честная позиция, что у прощания (04b): иначе нечего назвать.
        let FarewellPosition::Shown(position) = self.farewell_position() else {
            self.page_reopen.refuse_unsent();
            return;
        };
        match self.outbound.request_page_reopen(position) {
            ReopenRequestOutcome::Queued => {
                info!(
                    position_us = position.as_micros(),
                    "Asking the extension to bring the page back"
                );
                self.page_reopen.stage = PageReopenStage::Requested {
                    deadline: now + self.page_reopen.policy.patience,
                };
                self.page_reopen.notice = PageReopenNotice::Reopening;
            }
            outcome @ (ReopenRequestOutcome::NotAttachedYet
            | ReopenRequestOutcome::ProtocolTooOld) => {
                warn!(?outcome, "The page reopen request could not be queued");
                self.page_reopen.refuse_unsent();
            }
        }
    }

    /// Поток приёма разобрал всё, что экземпляр успел прислать, и запечатал его вход.
    ///
    /// Для сироты это и есть «связь закрылась»: уведомление идёт в порядке кадров, ПОСЛЕ
    /// последнего слова экстеншена. Событие сервера `Ended` (оно обгоняет данные) решать
    /// это не может: иначе отказ с причиной (`page_reopen_failed`), пришедший перед
    /// закрытием, стал бы безымянной «связь потеряна».
    pub(super) fn note_instance_sealed(&mut self, session: SessionRef) {
        self.sealed_instance = Some(session);
        if self.page_reopen.link_is_open() {
            self.page_link_closed();
        }
    }

    /// Связь сироты закрылась: вернуть страницу больше некому.
    fn page_link_closed(&mut self) {
        if matches!(self.page_reopen.stage, PageReopenStage::Refused(_)) {
            return;
        }
        if self.page_reopen.link_is_open() {
            self.page_reopen.refuse(PageReopenRefusal::LinkLost);
        }
        self.outbound.forget_pending_of_ended_session();
    }

    /// Срок ожидания ответа вышел (зовёт каждый шаг цикла).
    pub(super) fn expire_page_reopen(&mut self, now: Instant) {
        if self
            .page_reopen
            .deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            self.page_reopen.refuse(PageReopenRefusal::NoAnswer);
            self.outbound.forget_pending_of_ended_session();
        }
    }

    /// Слышит ли страница часы: она на месте или ушла, но связь держится (пульс сироты —
    /// он же говорит экстеншену секунду плеера).
    pub(super) const fn page_hears_clock(&self) -> bool {
        self.drain.is_none() || self.page_reopen.link_is_open()
    }

    /// Правило «пока просьба в пути, окно не закрывается»: конец досмотра (скачанное
    /// кончилось, застой, сбой) превращается в ожидание до срока просьбы.
    pub(super) fn hold_drain_for_page_reopen(
        &self,
        progress: DrainProgress,
        now: Instant,
    ) -> PageReopenHold {
        match (progress, self.page_reopen.deadline()) {
            (DrainProgress::Drained(_), Some(deadline)) => {
                PageReopenHold::Held(DrainProgress::Draining {
                    wake_at: deadline.max(now),
                })
            }
            (progress, _) => PageReopenHold::NotHeld(progress),
        }
    }

    /// Сообщение о возврате страницы за шаг, о котором окно ещё не сказало (забирает его).
    pub(crate) fn take_page_reopen_notice(&mut self) -> PageReopenNotice {
        std::mem::take(&mut self.page_reopen.notice)
    }

    /// Стадия возврата (для тестов и журнала).
    #[cfg(test)]
    pub(crate) const fn page_reopen_stage(&self) -> PageReopenStage {
        self.page_reopen.stage
    }
}

/// Итог правила «пока просьба в пути, окно не закрывается».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageReopenHold {
    /// Досмотр кончился бы, но просьба в пути: ждём до срока, окно говорит «Reopening».
    Held(DrainProgress),
    /// Правило не вмешалось: ответ досмотра как есть.
    NotHeld(DrainProgress),
}
