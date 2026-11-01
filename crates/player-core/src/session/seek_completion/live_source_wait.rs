//! Срок перемотки не идёт, пока источник честно ждёт живого поставщика
//! (решение владельца 150, мост в браузер), но не дольше предела молчания
//! (решение 151).
//!
//! **Контракт.** Демуксер сам говорит, чего ждёт, когда отвечает
//! `TemporarilyUnavailable` ([`media_core::Demuxer::data_wait`]):
//! [`media_core::DemuxDataWait::LiveProducer`] значит «недостающие данные несёт
//! поставщик, который ещё на связи» (страница браузера не ушла, поток не оборван), и
//! несёт счётчик того, что поставщик уже принёс для ожидаемых данных. Плеер ничего не
//! знает ни о браузере, ни о полке — только этот ответ.
//!
//! **Правило срока.** Перемотка сдаётся («Seeking timed out») в момент
//! `max(начало + C, min(последнее ожидание + C, последний рост принесённого + S))`, где
//! C — `player.seek.commit_timeout_ms` (10 с), S —
//! `player.seek.live_source_stall_timeout_ms` (60 с); моменты раньше начала перемотки
//! не засчитываются. По-человечески:
//! - обычный источник (ответов «живой поставщик» нет) — как раньше, C от начала;
//! - пока чтения источника отдают «данных нет, их несёт живой поставщик», срок C всё
//!   время начинается заново: спиннер, а как только данные пришли — посадка и игра;
//! - но если за S ни одного нового байта для ожидаемого места не пришло (страница на
//!   связи, а цель не несёт: держит байты у себя или проигнорировала перемотку) —
//!   честный отказ; новый клик или Play повторяют попытку;
//! - данные пришли, а застряли у плеера (декодер, звук, собственные очереди): чтений с
//!   «данных нет» больше нет, отметка не обновляется, и через полный C защита
//!   срабатывает — настоящий сбой не прячется. Блокировщики `audio_preroll` и
//!   `decoder_output` ждут так же, только пока источнику не хватает данных (сборка
//!   «видео + звук» иначе не ответила бы «данных нет»).
//!
//! Отметки ставит `demux_retry` на каждом чтении, отдавшем «данных нет», пока перемотка
//! активна. Поставщик ушёл — источник перестаёт отвечать `LiveProducer`, дальше конец
//! данных идёт обычным путём (досмотр, `EndOfStream`).

use std::time::{Duration, Instant};

use crate::seek_state::SeekCommitState;

use super::super::PlayerSession;

/// Сроки перемотки, из которых считается момент отказа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::session) struct SeekCommitBudget {
    /// Срок защиты перемотки (`player.seek.commit_timeout_ms`).
    pub(in crate::session) commit_timeout: Duration,
    /// Предел молчания живого поставщика (`player.seek.live_source_stall_timeout_ms`).
    pub(in crate::session) live_source_stall_timeout: Duration,
}

impl PlayerSession {
    /// Когда перемотка `seek_commit` сдаётся (правило — в описании модуля). `None` —
    /// никогда: сроки из config-а так велики, что момент не помещается в `Instant`.
    #[must_use]
    pub(in crate::session) fn seek_commit_deadline(
        &self,
        seek_commit: SeekCommitState,
        budget: SeekCommitBudget,
    ) -> Option<Instant> {
        let started_at = seek_commit.started_at;
        let protected_until = started_at.checked_add(budget.commit_timeout);
        let Some(wait) = self.live_producer_wait() else {
            return protected_until;
        };
        // Ответ до начала этой перемотки к ней не относится.
        if wait.last_wait_at < started_at {
            return protected_until;
        }
        let after_last_wait = wait.last_wait_at.checked_add(budget.commit_timeout);
        let stall_deadline = wait
            .last_progress_at
            .max(started_at)
            .checked_add(budget.live_source_stall_timeout);
        let live_deadline = earliest(after_last_wait, stall_deadline);
        latest(protected_until, live_deadline)
    }
}

/// Раньший из двух моментов (`None` — «никогда»).
fn earliest(first: Option<Instant>, second: Option<Instant>) -> Option<Instant> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.min(second)),
        (Some(moment), None) | (None, Some(moment)) => Some(moment),
        (None, None) => None,
    }
}

/// Поздний из двух моментов (`None` — «никогда», он позже любого).
fn latest(first: Option<Instant>, second: Option<Instant>) -> Option<Instant> {
    first.zip(second).map(|(first, second)| first.max(second))
}
