//! Wakeup worker-а к PTS звука, удержанного до своего времени.
//!
//! Удержанный звук (начало отрезка позже базы или разрыв посреди файла,
//! `session/audio_gap.rs`) входит, когда показ доходит до его PTS. Обычно worker и так
//! просыпается к кадрам, но если видео кончилось раньше звука (видео 0–2 с, звук с 3 с),
//! кадров впереди нет, и worker спал бы грубым интервалом: вход опоздал бы, а звук до
//! позиции входа обрезался бы. Поэтому план wakeup-а берёт и дедлайн звука, если тот
//! раньше уже выбранного.
//!
//! Причина плана — `PipelineWorkReady` с задержкой, как у дедлайна пополнения audio
//! buffer-а: в этот момент audio work становится выполнимым. Отдельной причины нет
//! намеренно: `diagnostics.rs` — замороженный legacy-модуль (S42 guardrail).

use std::time::Instant;

use super::wakeup::PlayerWorkerWakeupPlan;
use crate::WorkerWakeupReason;
use crate::session::PlayerSession;

/// Возвращает план с дедлайном удержанного звука, если он строго раньше `existing_plan`.
pub(super) fn prefer_earlier_held_audio_start(
    session: &PlayerSession,
    now: Instant,
    existing_plan: PlayerWorkerWakeupPlan,
) -> PlayerWorkerWakeupPlan {
    let Some(audio_delay) = session.held_audio_start_wall_delay(now) else {
        return existing_plan;
    };
    if existing_plan
        .delay
        .is_some_and(|delay| delay <= audio_delay)
    {
        return existing_plan;
    }
    PlayerWorkerWakeupPlan {
        delay: Some(audio_delay),
        reason: WorkerWakeupReason::PipelineWorkReady,
        frame_timing: existing_plan.frame_timing,
        wait_for_decoder_activity: false,
    }
}
