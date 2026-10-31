//! Покадровая отложенная запись runtime-originated настроек в config.
//!
//! Три независимых источника: ширина sidebar (drag мышью), громкость пользователя
//! (UX-11: слайдер, M, MPRIS) и выбор «субтитры включены» (CC, клавиша C). Все только запоминают изменение и пишут config после
//! паузы через `SettingsRuntime`; сам frame pipeline не знает про debounce.
//!
//! Здесь же — шаг «общей доски» (04c): принятие настроек, которые записала соседняя копия
//! плеера. Он идёт последним: сначала своё отложенное пишется в файл (под замком, с
//! перечитыванием), потом копия смотрит, что на диске нового от соседей. Пока принятие
//! ждёт маршрут, отложенные записи выше сами ждут его конца (как во время Apply).

use std::time::Instant;

use crate::settings_runtime::SettingsRuntime;
use crate::state::SubtitleVisibility;
use crate::ui::sidebar::SidebarWidthChange;
use crate::user_audio_level::PlayerAudioObservation;

use super::FrameSettingsRuntimeAdapter;

/// Учитывает изменения кадра и пишет то, у чего пауза уже прошла.
///
/// Возвращает `true`, если попытка записи завершилась и UI нужно перерисовать.
/// Ошибки не глотаются: они попадают в status окна настроек.
pub(super) fn persist_due_runtime_originated_settings(
    settings_runtime: &mut SettingsRuntime,
    sidebar_width_change: Option<SidebarWidthChange>,
    player_audio: PlayerAudioObservation,
    subtitle_enabled_change: Option<SubtitleVisibility>,
    runtime_adapter: &mut FrameSettingsRuntimeAdapter<'_>,
) -> bool {
    let now = Instant::now();
    if let Some(width_change) = sidebar_width_change {
        let _pending_changed = settings_runtime.record_sidebar_width_change(width_change, now);
    }
    let _audio_level_changed = settings_runtime.record_player_audio_observation(player_audio, now);
    let _subtitles_choice_changed =
        settings_runtime.record_subtitle_enabled_change(subtitle_enabled_change, now);

    let resize_requested_repaint =
        match settings_runtime.flush_due_sidebar_resize(now, runtime_adapter) {
            Ok(outcome) => outcome.needs_redraw(),
            Err(error) => {
                settings_runtime.report_runtime_error("Could not save sidebar width", error);
                true
            }
        };
    let audio_requested_repaint =
        match settings_runtime.flush_due_user_audio_level(now, runtime_adapter) {
            Ok(outcome) => outcome.needs_redraw(),
            Err(error) => {
                settings_runtime.report_runtime_error("Could not save volume", error);
                true
            }
        };
    let subtitles_requested_repaint =
        match settings_runtime.flush_due_subtitle_enabled(now, runtime_adapter) {
            Ok(outcome) => outcome.needs_redraw(),
            Err(error) => {
                settings_runtime.report_runtime_error("Could not save subtitles choice", error);
                true
            }
        };
    let board_requested_repaint =
        adopt_settings_from_other_windows(settings_runtime, runtime_adapter, now);
    resize_requested_repaint
        || audio_requested_repaint
        || subtitles_requested_repaint
        || board_requested_repaint
}

/// Шаг «общей доски»: опрос принятия в полёте или разбор нового сигнала слежения.
///
/// Уведомление (битый файл, отказ принятия) показывается в центре уведомлений этого окна.
fn adopt_settings_from_other_windows(
    settings_runtime: &mut SettingsRuntime,
    runtime_adapter: &mut FrameSettingsRuntimeAdapter<'_>,
    now: Instant,
) -> bool {
    let board_frame = settings_runtime.advance_shared_settings_board(runtime_adapter, now);
    if board_frame.session_only_lifted {
        // Файл снова нормальный: «изменения не сохранятся» больше неправда.
        runtime_adapter.withdraw_settings_file_write_notices();
    }
    if let Some(notice) = board_frame.notice {
        runtime_adapter.show_settings_file_notice(notice);
    }
    // «Применено, но в файл пока не записано» — от любой записи этого кадра или прошлого
    // (Apply, громкость, сайдбар, дописывание): одно уведомление на причину.
    let save_notice_shown = match settings_runtime.take_settings_save_notice() {
        Some(save_notice) => {
            runtime_adapter.show_settings_save_notice(save_notice);
            true
        }
        None => false,
    };
    board_frame.needs_redraw || save_notice_shown
}
