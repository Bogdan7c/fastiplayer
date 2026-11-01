//! Wakeup worker-а к концу доступного у обрезанных данных (`eof_drain/cut_short_stop.rs`).
//!
//! Обычно worker просыпается к кадрам и пополнению звука. Но когда видео уже кончилось,
//! а звук, прочитанный впрок, играет дальше конца доступного, кадров впереди нет, и
//! worker спал бы грубым интервалом: звук звучал бы лишние сотни миллисекунд после
//! конца картинки. Поэтому план wakeup-а берёт и этот дедлайн, если он раньше уже
//! выбранного.
//!
//! Причина плана — `PipelineWorkReady` (в этот момент работа шага становится
//! выполнимой), как у дедлайна удержанного звука: отдельной причины нет намеренно —
//! `diagnostics.rs` — замороженный legacy-модуль (S42 guardrail).

use std::time::Instant;

use super::wakeup::PlayerWorkerWakeupPlan;
use crate::WorkerWakeupReason;
use crate::session::PlayerSession;

/// Возвращает план с дедлайном конца доступного, если он строго раньше `existing_plan`.
pub(super) fn prefer_earlier_cut_short_end(
    session: &PlayerSession,
    now: Instant,
    existing_plan: PlayerWorkerWakeupPlan,
) -> PlayerWorkerWakeupPlan {
    let Some(end_delay) = session.cut_short_playable_end_wall_delay(now) else {
        return existing_plan;
    };
    if existing_plan.delay.is_some_and(|delay| delay <= end_delay) {
        return existing_plan;
    }
    PlayerWorkerWakeupPlan {
        delay: Some(end_delay),
        reason: WorkerWakeupReason::PipelineWorkReady,
        frame_timing: existing_plan.frame_timing,
        wait_for_decoder_activity: false,
    }
}
