//! Действия со строками меню субтитров страницы на стороне `AppState` (сессия 15a2).
//!
//! # Границы
//! - Меню страницы и отказ в рекламе принадлежат окну (`state::window_mode::page_variants`):
//!   выбор строки идёт ровно тем путём, что выбор озвучки, — намерением в ящик медиа-части
//!   сеанса, и там уходит странице как `select_variant{subtitles}`.
//! - «Жду документ этой строки» принадлежит владельцу субтитров (`page_line_wait`): когда
//!   документ придёт, он сам станет выбором зрителя. Здесь ожидание только ставится — и только
//!   если намерение действительно ушло (отказ в рекламе, устаревшее меню ожидания не рождают).
//! - Клавиша V при меню страницы ходит по родным строкам ([`next_native_line_step`]).

use super::SubtitleOwner;
use super::control_actions::ControlActionFeedback;
use super::page_lines::{NativeLineStep, next_native_line_step, page_toggle_step};
use crate::page_variants::PageVariantChoice;
use crate::state::AppState;
use crate::state::PageVariantSelectOutcome;
use crate::state::notifications::TransientReplaceKey;
use crate::subtitle_message::{SUBTITLES_OFF_MESSAGE, track_selected_toast};
use crate::ui::player_controls::SubtitleControlAction;

impl AppState {
    /// Зритель выбрал строку меню субтитров страницы (клик по строке, клавиша V).
    ///
    /// Прежний показ не трогается: он сменится, только когда придёт документ строки.
    pub(crate) fn select_page_subtitle_line(
        &mut self,
        choice: PageVariantChoice,
    ) -> PageVariantSelectOutcome {
        let outcome = self.select_page_variant(choice);
        expect_document_if_queued(outcome, &mut self.subtitles);
        outcome
    }

    /// Есть ли у окна меню субтитров страницы (клавиша V тогда ходит по его строкам).
    pub(in crate::state) fn has_page_subtitle_lines(&self) -> bool {
        self.page_variant_facts().subtitles.is_some()
    }

    /// Клавиша C при меню страницы без единой дорожки: включить первую родную строку
    /// ([`page_toggle_step`]); `None` — C работает как раньше, у владельца субтитров.
    pub(in crate::state) fn toggle_page_subtitles_without_tracks(
        &mut self,
    ) -> Option<ControlActionFeedback> {
        let view = self.subtitle_menu_view();
        let step = page_toggle_step(&view.page_lines, view.has_tracks)?;
        Some(self.apply_page_line_step(step))
    }

    /// Клавиша V при меню страницы: следующая родная строка, затем «выкл», затем первая.
    pub(in crate::state) fn cycle_page_subtitle_line(&mut self) -> ControlActionFeedback {
        let view = self.subtitle_menu_view();
        self.apply_page_line_step(next_native_line_step(&view.page_lines))
    }

    /// Исполнить шаг по строкам страницы: выбрать строку (с тостом, если просьба ушла) или выключить.
    fn apply_page_line_step(&mut self, step: NativeLineStep) -> ControlActionFeedback {
        let view = self.subtitle_menu_view();
        match step {
            NativeLineStep::Select(position) => {
                let Some(line) = view.page_lines.get(position) else {
                    return ControlActionFeedback::Silent;
                };
                match self.select_page_subtitle_line(line.choice) {
                    PageVariantSelectOutcome::Queued => ControlActionFeedback::ReplacingToast {
                        key: TransientReplaceKey::SubtitleTrack,
                        text: track_selected_toast(&line.label),
                    },
                    // Реклама, устаревшее меню и прочее уже в журнале окна; тост не нужен.
                    _ => ControlActionFeedback::Silent,
                }
            }
            NativeLineStep::TurnOff => {
                let _ = self
                    .subtitles
                    .apply_control_action(SubtitleControlAction::Disable);
                ControlActionFeedback::ReplacingToast {
                    key: TransientReplaceKey::SubtitleTrack,
                    text: SUBTITLES_OFF_MESSAGE.to_owned(),
                }
            }
            NativeLineStep::NothingToSelect => ControlActionFeedback::Silent,
        }
    }

    /// Зритель запросил следующую серию: ожидание документа прежней серии больше не нужно.
    pub(in crate::state) fn cancel_page_document_wait(&mut self) {
        self.subtitles.cancel_page_document_wait();
    }
}

/// Ожидание документа ставится только когда просьба действительно ушла в ящик сеанса: отказ в
/// рекламе, устаревшее меню или обычное окно ожидания не рождают (иначе чужой документ,
/// пришедший позже, стал бы «выбором зрителя»).
pub(super) fn expect_document_if_queued(
    outcome: PageVariantSelectOutcome,
    owner: &mut SubtitleOwner,
) {
    if outcome == PageVariantSelectOutcome::Queued {
        owner.expect_page_document();
    }
}
