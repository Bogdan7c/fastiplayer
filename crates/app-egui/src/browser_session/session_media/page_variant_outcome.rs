//! Исход выбора зрителя на странице (протокол 1.8, сессия 15c2, решения 295, 298, 301).
//!
//! ```text
//!  окно: выбор зрителя ─► viewer_selects_page_variant ─► select_variant странице
//!        └─ after_selection_sent: помнит «ожидающий выбор» (вид + variant_id + подпись),
//!           взводит возврат секунды и говорит окну «Switching … on the page…» (1.8+)
//!  страница (1.8+): variant_outcome{request, outcome}
//!        ├─ не наш ожидающий выбор (чужой, устаревший, повторный) → ничего не меняем
//!        ├─ done           → тост «Audio: …» / «Quality: …» (прежний текст, теперь правда)
//!        └─ отказ          → тост с причиной; возврат секунды снят, ЕСЛИ этот выбор
//!                            последним его взвёл (иначе отказ старой озвучки убил бы
//!                            возврат более нового выбора качества)
//! ```
//!
//! **Версии.** Общая версия ниже 1.8: страница исхода не пришлёт, ничего не ожидается, зритель
//! сразу видит «Audio: …» (поведение 1.7). Решает это вопрос владельца исходящих
//! (`PageOutbound::variant_outcome_reporting`), а не поле версии здесь.
//!
//! **Чего исход не трогает.** Галочка в меню ♪ идёт только за меню страницы (`склад`): исход её
//! не двигает. Выбор субтитров поколения не меняет: для него исход — одна строка журнала.
//!
//! **Журнал** — без подписей (текст страницы): `Page variant outcome kind=audio
//! outcome=no_such_episode matched=true`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use browser_bridge_protocol::session::{VariantOutcome, VariantRequest};
use browser_bridge_protocol::text::VariantId;
use tracing::debug;
use tracing::info;

use super::SessionMedia;
use super::position_carry::{CarryArming, CarryEnd};
use crate::browser_session::outbound::VariantOutcomeReporting;
use crate::browser_session::page_variants::{PageVariantOutcomeWord, page_variant_kind};
use crate::page_variants::{
    PageVariantKind, PageVariantNotice, PageVariantRefusal, PageVariantSubject,
};

/// Строка журнала о каждом полученном исходе (читают стенд и тесты; дальше `kind=… outcome=…
/// matched=…`).
pub(crate) const PAGE_VARIANT_OUTCOME_LINE: &str = "Page variant outcome";

/// Выбор зрителя, который ушёл странице и ждёт исхода.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SentSelection {
    /// Пункт, который просили у страницы (по нему сверяется исход).
    variant_id: VariantId,
    /// Подпись пункта для тоста «Audio: …» (очищенная).
    label: Arc<str>,
}

/// Чей выбор последним взвёл возврат секунды.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CarryOwner {
    kind: PageVariantKind,
    variant_id: VariantId,
}

/// На какую ожидающую просьбу пришёл исход (уже снятую с ожидания).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AnsweredRequest {
    /// Выбор пункта меню.
    Selection {
        /// Вид меню.
        kind: PageVariantKind,
        /// Подпись пункта.
        label: Arc<str>,
        /// Этот выбор последним взвёл возврат секунды.
        owned_the_carry: bool,
    },
    /// Просьба о следующей серии.
    NextEpisode,
}

/// Ожидающие исхода просьбы зрителя в текущем сеансе (по одной на вид меню и Next).
///
/// Новый выбор того же вида вытесняет прежний: страница ответит на оба, но первый ответ уже
/// устарел и совпадения не даст.
#[derive(Debug, Default)]
pub(super) struct PendingVariantRequests {
    selections: HashMap<PageVariantKind, SentSelection>,
    next_episode: bool,
    carry_owner: Option<CarryOwner>,
}

impl PendingVariantRequests {
    /// Выбор ушёл странице и ждёт исхода (вытесняет прежний того же вида).
    pub(super) fn remember_selection(
        &mut self,
        kind: PageVariantKind,
        variant_id: VariantId,
        label: Arc<str>,
    ) {
        self.selections
            .insert(kind, SentSelection { variant_id, label });
    }

    /// Этот выбор только что взвёл возврат секунды (последний взведший — владелец возврата).
    pub(super) fn remember_carry_owner(&mut self, kind: PageVariantKind, variant_id: VariantId) {
        self.carry_owner = Some(CarryOwner { kind, variant_id });
    }

    /// Просьба о следующей серии ушла странице и ждёт исхода. Следующая серия начинается
    /// сначала и возврат секунды снимает, поэтому владельца возврата больше нет.
    pub(super) fn remember_next_episode(&mut self) {
        self.next_episode = true;
        self.carry_owner = None;
    }

    /// Новый сеанс: ожидания прежней страницы к новой не относятся.
    pub(super) fn clear(&mut self) {
        self.selections.clear();
        self.next_episode = false;
        self.carry_owner = None;
    }

    /// Снимает с ожидания просьбу, на которую ответила страница. `None` — это не наша
    /// ожидающая просьба (чужая, устаревшая или уже отвеченная): ничего не меняется.
    pub(super) fn take_matching(&mut self, request: &VariantRequest) -> Option<AnsweredRequest> {
        match request {
            VariantRequest::NextEpisode {} => {
                std::mem::take(&mut self.next_episode).then_some(AnsweredRequest::NextEpisode)
            }
            VariantRequest::SelectVariant { menu, variant_id } => {
                let kind = page_variant_kind(*menu);
                if self.selections.get(&kind)?.variant_id != *variant_id {
                    return None;
                }
                let sent = self.selections.remove(&kind)?;
                let owned_the_carry = self
                    .carry_owner
                    .as_ref()
                    .is_some_and(|owner| owner.kind == kind && owner.variant_id == *variant_id);
                if owned_the_carry {
                    self.carry_owner = None;
                }
                Some(AnsweredRequest::Selection {
                    kind,
                    label: sent.label,
                    owned_the_carry,
                })
            }
        }
    }
}

impl SessionMedia {
    /// Выбор зрителя ушёл странице: запомнить ожидание, взвести возврат секунды и решить, что
    /// сказать окну (правила — в описании модуля).
    pub(super) fn after_selection_sent(
        &mut self,
        kind: PageVariantKind,
        variant_id: VariantId,
        label: Arc<str>,
        now: Instant,
    ) {
        let reporting = self.outbound.variant_outcome_reporting();
        if reporting == VariantOutcomeReporting::PageReports {
            self.variants
                .pending
                .remember_selection(kind, variant_id.clone(), label.clone());
        }
        // Субтитры поколения не меняют (страница лишь скачивает ещё один документ): ни
        // возврата секунды, ни тоста о смене варианта, иначе взведённый возврат дождался бы
        // чужого поколения (например, после рекламы) и сдвинул бы видео.
        if !kind.selection_restarts_media() {
            return;
        }
        // Решение 280: новое поколение после смены встанет на секунду зрителя.
        let arming = self.arm_position_carry(now);
        self.variants.notice = Some(match reporting {
            VariantOutcomeReporting::PageReports => {
                // Владелец возврата — только тот, кто его действительно взвёл.
                if arming == CarryArming::Armed {
                    self.variants.pending.remember_carry_owner(kind, variant_id);
                }
                PageVariantNotice::Switching { kind }
            }
            VariantOutcomeReporting::PageStaysSilent => PageVariantNotice::Selected { kind, label },
        });
    }

    /// Просьба о следующей серии ушла странице: ждать исхода, если страница его пришлёт.
    pub(super) fn after_next_episode_sent(&mut self) {
        if self.outbound.variant_outcome_reporting() == VariantOutcomeReporting::PageReports {
            self.variants.pending.remember_next_episode();
        }
    }

    /// Исход пришёл от страницы (`variant_outcome`).
    pub(super) fn apply_variant_outcome(&mut self, word: PageVariantOutcomeWord) {
        let PageVariantOutcomeWord { request, outcome } = word;
        let answered = self.variants.pending.take_matching(&request);
        info!(
            "{PAGE_VARIANT_OUTCOME_LINE} kind={} outcome={} matched={}",
            request_log_label(&request),
            outcome_log_label(outcome),
            answered.is_some()
        );
        match answered {
            None => debug!("Page variant outcome is not about a pending request; ignored"),
            Some(AnsweredRequest::Selection {
                kind,
                label,
                owned_the_carry,
            }) => self.finish_selection(kind, label, owned_the_carry, outcome),
            Some(AnsweredRequest::NextEpisode) => {
                if let Some(reason) = refusal_of(outcome) {
                    self.variants.notice = Some(PageVariantNotice::Refused {
                        subject: PageVariantSubject::NextEpisode,
                        reason,
                    });
                }
            }
        }
    }

    /// Исход выбора пункта меню: тост и судьба возврата секунды.
    fn finish_selection(
        &mut self,
        kind: PageVariantKind,
        label: Arc<str>,
        owned_the_carry: bool,
        outcome: VariantOutcome,
    ) {
        if !kind.selection_restarts_media() {
            // Субтитры: тостов и возврата не было, исход только в журнале.
            return;
        }
        match refusal_of(outcome) {
            None => self.variants.notice = Some(PageVariantNotice::Selected { kind, label }),
            Some(reason) => {
                // Возврат снимает только отказ ТОГО выбора, что взвёл его последним: отказ
                // старого выбора не должен убить возврат более нового.
                if owned_the_carry {
                    self.forget_position_carry(CarryEnd::PageRefusedSelection);
                }
                self.variants.notice = Some(PageVariantNotice::Refused {
                    subject: PageVariantSubject::Menu(kind),
                    reason,
                });
            }
        }
    }
}

/// Отказ страницы (`None` — страница выполнила просьбу).
const fn refusal_of(outcome: VariantOutcome) -> Option<PageVariantRefusal> {
    match outcome {
        VariantOutcome::Done => None,
        VariantOutcome::NoSuchEpisode => Some(PageVariantRefusal::NoSuchEpisode),
        VariantOutcome::PersonOnly => Some(PageVariantRefusal::PersonOnly),
        VariantOutcome::Failed => Some(PageVariantRefusal::Failed),
    }
}

/// Слово исхода для журнала (то же, что на проводе).
const fn outcome_log_label(outcome: VariantOutcome) -> &'static str {
    match outcome {
        VariantOutcome::Done => "done",
        VariantOutcome::NoSuchEpisode => "no_such_episode",
        VariantOutcome::PersonOnly => "person_only",
        VariantOutcome::Failed => "failed",
    }
}

/// Слово просьбы для журнала: вид меню или `next_episode` (без variant_id и подписей).
const fn request_log_label(request: &VariantRequest) -> &'static str {
    match request {
        VariantRequest::SelectVariant { menu, .. } => page_variant_kind(*menu).log_label(),
        VariantRequest::NextEpisode {} => "next_episode",
    }
}
