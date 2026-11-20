//! Строка замера смены медиа в журнале — стабильный текстовый контракт для судьи стенда.
//!
//! Формат (одна строка уровня info на смену, все поля всегда, порядок постоянный):
//!
//! ```text
//! Browser window media switch switch=2 end=first_frame outcome=held release=none empty_frames=0 held_frames=7 first_frame_ms=184 render_generation=5
//! ```
//!
//! - `switch` — номер смены в окне (1 — первое медиа окна);
//! - `end` — `first_frame` (показан кадр нового медиа) или причина обрыва: `load_refused`,
//!   `load_failed`, `player_stopped`, `player_failed`, `player_shutdown`, `player_fatal_error`,
//!   `media_released`, `media_reset`, `audio_only_media`;
//! - `outcome` — что стало с прежним кадром: `held` (держался до конца смены), `expired`
//!   (вышел срок), `released` (отпущен жёсткой причиной, она в `release`), `disabled`
//!   (политика окна не держит), `no_previous_frame` (держать было нечего);
//! - `release` — причина `released`: `surface_lifecycle_break`, `render_failure`,
//!   `worker_render_error`, `backend_swap`, `renderer_replaced`; иначе `none`;
//! - `empty_frames` — кадров окна без видео (пустое поле) на экране за смену;
//! - `held_frames` — кадров окна с удержанным прежним кадром на экране;
//! - `first_frame_ms` — мс от начала смены до показа кадра нового медиа (`none` при обрыве);
//! - `render_generation` — поколение рендера нового медиа (`none` при обрыве). Номер
//!   поколения страницы на этом уровне неизвестен: его пишет строка `Browser stream generation
//!   shown generation=N` медиа-части, она идёт в журнале рядом.
//!
//! Текст журнала английский (`mem:app-egui/user-facing-language`).

use tracing::info;

use super::{
    HoldReleaseCause, MediaSwitchAbandonCause, MediaSwitchEnd, MediaSwitchHoldOutcome,
    MediaSwitchReport,
};

/// Слово поля `end`.
const fn end_word(end: MediaSwitchEnd) -> &'static str {
    match end {
        MediaSwitchEnd::FirstNewFrame { .. } => "first_frame",
        MediaSwitchEnd::Abandoned(cause) => abandon_word(cause),
    }
}

/// Слово причины обрыва смены.
const fn abandon_word(cause: MediaSwitchAbandonCause) -> &'static str {
    match cause {
        MediaSwitchAbandonCause::LoadRefused => "load_refused",
        MediaSwitchAbandonCause::LoadFailed => "load_failed",
        MediaSwitchAbandonCause::PlayerStopped => "player_stopped",
        MediaSwitchAbandonCause::PlayerFailed => "player_failed",
        MediaSwitchAbandonCause::PlayerShutdown => "player_shutdown",
        MediaSwitchAbandonCause::PlayerFatalError => "player_fatal_error",
        MediaSwitchAbandonCause::MediaReleased => "media_released",
        MediaSwitchAbandonCause::MediaReset => "media_reset",
        MediaSwitchAbandonCause::AudioOnlyMedia => "audio_only_media",
    }
}

/// Слово поля `outcome`.
const fn outcome_word(outcome: MediaSwitchHoldOutcome) -> &'static str {
    match outcome {
        MediaSwitchHoldOutcome::Held => "held",
        MediaSwitchHoldOutcome::Expired => "expired",
        MediaSwitchHoldOutcome::Released(_) => "released",
        MediaSwitchHoldOutcome::Disabled => "disabled",
        MediaSwitchHoldOutcome::NoPreviousFrame => "no_previous_frame",
    }
}

/// Слово поля `release`.
const fn release_word(outcome: MediaSwitchHoldOutcome) -> &'static str {
    match outcome {
        MediaSwitchHoldOutcome::Released(cause) => match cause {
            HoldReleaseCause::SurfaceLifecycleBreak => "surface_lifecycle_break",
            HoldReleaseCause::RenderFailure => "render_failure",
            HoldReleaseCause::WorkerRenderError => "worker_render_error",
            HoldReleaseCause::BackendSwap => "backend_swap",
            HoldReleaseCause::RendererReplaced => "renderer_replaced",
        },
        MediaSwitchHoldOutcome::Held
        | MediaSwitchHoldOutcome::Expired
        | MediaSwitchHoldOutcome::Disabled
        | MediaSwitchHoldOutcome::NoPreviousFrame => "none",
    }
}

/// Поля строки замера в порядке контракта (без заголовка): `key=value` через пробел.
pub(super) fn media_switch_report_fields(report: &MediaSwitchReport) -> String {
    let (first_frame_ms, render_generation) = match report.end {
        MediaSwitchEnd::FirstNewFrame {
            after,
            render_generation,
        } => (after.as_millis().to_string(), render_generation.to_string()),
        MediaSwitchEnd::Abandoned(_) => ("none".to_owned(), "none".to_owned()),
    };
    format!(
        "switch={} end={} outcome={} release={} empty_frames={} held_frames={} first_frame_ms={} render_generation={}",
        report.switch_number,
        end_word(report.end),
        outcome_word(report.hold_outcome),
        release_word(report.hold_outcome),
        report.empty_frames,
        report.held_frames,
        first_frame_ms,
        render_generation,
    )
}

/// Пишет строку замера одной смены.
pub(super) fn log_media_switch_report(report: &MediaSwitchReport) {
    info!(
        "Browser window media switch {}",
        media_switch_report_fields(report)
    );
}
