//! Central preview staged S08 playlist import-а.
//!
//! Модуль читает immutable counts/issues и возвращает typed post-render action.
//! Parser, filesystem, queue mutation и confirmation ownership здесь отсутствуют.

use crate::playlist_runtime::{
    PlaylistImportIntent, PlaylistImportIssueKind, PlaylistImportPreview,
    PlaylistImportRejectedCount,
};

use super::PlaylistAction;

/// Рисует единственный authoritative preview и возвращает не более одного intent-а.
pub(crate) fn render(ui: &mut egui::Ui, preview: &PlaylistImportPreview) -> Option<PlaylistAction> {
    render_preview(ui, preview).action
}

/// Test-visible geometry остаётся внутренней деталью renderer-а.
struct ImportPreviewRenderResult {
    action: Option<PlaylistAction>,
    #[cfg(test)]
    cancel_rect: egui::Rect,
    #[cfg(test)]
    continue_rect: egui::Rect,
    #[cfg(test)]
    cancel_has_focus: bool,
    #[cfg(test)]
    continue_has_focus: bool,
}

/// Один render pass строит content и обе стандартные accessible egui buttons.
fn render_preview(ui: &mut egui::Ui, preview: &PlaylistImportPreview) -> ImportPreviewRenderResult {
    let mut action = None;
    let mut cancel_response = None;
    let mut continue_response = None;
    ui.vertical_centered(|ui| {
        ui.add_space(32.0);
        ui.heading(import_heading(preview.intent()));
        ui.add_space(8.0);

        let accepted = preview.accepted();
        ui.label(format!(
            "Будет добавлено: {} элементов, включая {} групп ({} медиа).",
            accepted.singles().saturating_add(accepted.groups()),
            accepted.groups(),
            accepted.retained_items(),
        ));

        if preview.requires_partial_decision() {
            ui.add_space(8.0);
            ui.colored_label(
                egui::Color32::YELLOW,
                "Файл импортируется частично. Проверьте ограничения перед продолжением.",
            );
            render_issue_summary(ui, preview);
        }

        if preview.sensitive_durable_locator_count() > 0 {
            ui.add_space(6.0);
            ui.label(format!(
                "Чувствительных адресов для сохранения: {}. Следующим шагом потребуется подтверждение.",
                preview.sensitive_durable_locator_count(),
            ));
        }

        if preview.intent() == PlaylistImportIntent::ReplaceQueue {
            ui.add_space(6.0);
            ui.label(
                "Текущий плейлист будет заменён только после отдельного подтверждения; воспроизведение продолжится.",
            );
        }

        ui.add_space(14.0);
        ui.horizontal(|ui| {
            let response = ui.button("Отмена");
            if response.clicked() {
                action = Some(PlaylistAction::CancelImport(preview.preview_id()));
            }
            cancel_response = Some(response);
            let response = ui.button(if preview.requires_partial_decision() {
                "Продолжить частичный импорт"
            } else {
                "Импортировать"
            });
            if response.clicked() {
                action = Some(PlaylistAction::ContinueImport(preview.preview_id()));
            }
            continue_response = Some(response);
        });
    });
    let cancel_response = cancel_response.expect("preview всегда рисует Cancel");
    let continue_response = continue_response.expect("preview всегда рисует Continue");
    #[cfg(not(test))]
    let _ = (&cancel_response, &continue_response);
    ImportPreviewRenderResult {
        action,
        #[cfg(test)]
        cancel_rect: cancel_response.rect,
        #[cfg(test)]
        continue_rect: continue_response.rect,
        #[cfg(test)]
        cancel_has_focus: cancel_response.has_focus(),
        #[cfg(test)]
        continue_has_focus: continue_response.has_focus(),
    }
}

/// Heading повторяет explicit menu intent и не скрывает replace semantics.
const fn import_heading(intent: PlaylistImportIntent) -> &'static str {
    match intent {
        PlaylistImportIntent::AppendToQueue => "Добавить к плейлисту",
        PlaylistImportIntent::ReplaceQueue => "Открыть как новый плейлист",
        PlaylistImportIntent::StartupReplace => "Открыть плейлист",
    }
}

/// Показывает только bounded safe categories и доказанные counts.
fn render_issue_summary(ui: &mut egui::Ui, preview: &PlaylistImportPreview) {
    let rejected_sources = preview
        .issues()
        .iter()
        .filter(|issue| issue.kind() == PlaylistImportIssueKind::SourceRejectedEntry)
        .count();
    let unsupported_locators = preview
        .issues()
        .iter()
        .filter(|issue| issue.kind() == PlaylistImportIssueKind::UnsupportedLocator)
        .count();
    if rejected_sources > 0 {
        ui.label(format!(
            "Отклонённых или нераскрытых записей: {rejected_sources}."
        ));
    }
    if unsupported_locators > 0 {
        ui.label(format!(
            "Записей с неподдерживаемым адресом: {unsupported_locators}."
        ));
    }
    if preview.omitted_issue_count() > 0
        || preview
            .issues()
            .iter()
            .any(|issue| issue.kind() == PlaylistImportIssueKind::DiagnosticPrefixTruncated)
    {
        ui.label(format!(
            "Дополнительных диагностик вне краткого списка: не менее {}.",
            preview.omitted_issue_count(),
        ));
    }
    if let Some(source_truncation) = preview.source_truncation() {
        let text = match source_truncation.rejected_entries() {
            PlaylistImportRejectedCount::Exact(count) => {
                format!("Источник не отдал {count} записей из-за ограничений.")
            }
            PlaylistImportRejectedCount::AtLeast(count) => {
                format!("Источник не отдал как минимум {count} запись из-за ограничений.")
            }
        };
        ui.label(text);
    }
    if let Some(capacity) = preview.capacity_truncation() {
        ui.label(format!(
            "Лимит очереди исключил {} целых элементов ({} медиа); группы не разрезались.",
            capacity.rejected_entries(),
            capacity.rejected_items(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use egui::{Context, Event, Key, Modifiers, PointerButton, RawInput, Rect, pos2, vec2};

    use super::*;
    use crate::playlist_runtime::{
        PlaylistImportIssueKind, PlaylistImportPreviewUiAcceptedFixture,
        PlaylistImportPreviewUiCapacityFixture, PlaylistImportPreviewUiFixture,
    };

    fn preview_with_issues(intent: PlaylistImportIntent) -> PlaylistImportPreview {
        PlaylistImportPreview::for_ui_test(PlaylistImportPreviewUiFixture {
            intent,
            accepted: PlaylistImportPreviewUiAcceptedFixture {
                singles: 2,
                groups: 1,
                retained_items: 4,
            },
            issue_kinds: &[
                PlaylistImportIssueKind::SourceRejectedEntry,
                PlaylistImportIssueKind::UnsupportedLocator,
                PlaylistImportIssueKind::DiagnosticPrefixTruncated,
            ],
            source_rejected_at_least: Some(1),
            capacity_rejected: Some(PlaylistImportPreviewUiCapacityFixture {
                rejected_entries: 2,
                rejected_items: 3,
            }),
            sensitive_durable_locator_count: 1,
        })
    }

    fn raw_input(events: Vec<Event>) -> RawInput {
        RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 420.0))),
            events,
            ..RawInput::default()
        }
    }

    fn keyboard_input(key: Key) -> RawInput {
        raw_input(vec![
            Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
            Event::Key {
                key,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
        ])
    }

    fn pointer_button(position: egui::Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }
    }

    fn frame(
        context: &Context,
        preview: &PlaylistImportPreview,
        input: RawInput,
    ) -> ImportPreviewRenderResult {
        let mut result = None;
        let _ = context.run_ui(input, |ui| {
            result = Some(render_preview(ui, preview));
        });
        result.expect("preview result")
    }

    #[test]
    fn clean_issue_capacity_sensitive_and_replace_states_remain_distinct() {
        let clean = PlaylistImportPreview::for_ui_test(PlaylistImportPreviewUiFixture {
            intent: PlaylistImportIntent::AppendToQueue,
            accepted: PlaylistImportPreviewUiAcceptedFixture {
                singles: 2,
                groups: 0,
                retained_items: 2,
            },
            issue_kinds: &[],
            source_rejected_at_least: None,
            capacity_rejected: None,
            sensitive_durable_locator_count: 0,
        });
        let partial = preview_with_issues(PlaylistImportIntent::AppendToQueue);
        let replace = preview_with_issues(PlaylistImportIntent::ReplaceQueue);

        assert!(!clean.requires_partial_decision());
        assert!(partial.requires_partial_decision());
        assert_eq!(partial.capacity_truncation().unwrap().rejected_items(), 3);
        assert_eq!(replace.intent(), PlaylistImportIntent::ReplaceQueue);
        assert_eq!(replace.sensitive_durable_locator_count(), 1);
        assert_eq!(import_heading(clean.intent()), "Добавить к плейлисту");
        assert_eq!(
            import_heading(replace.intent()),
            "Открыть как новый плейлист"
        );
    }

    #[test]
    fn continue_supports_pointer_click() {
        let context = Context::default();
        let preview = preview_with_issues(PlaylistImportIntent::AppendToQueue);
        let initial = frame(&context, &preview, raw_input(Vec::new()));
        let position = initial.continue_rect.center();

        let _ = frame(
            &context,
            &preview,
            raw_input(vec![Event::PointerMoved(position)]),
        );
        let _ = frame(
            &context,
            &preview,
            raw_input(vec![pointer_button(position, true)]),
        );
        let released = frame(
            &context,
            &preview,
            raw_input(vec![pointer_button(position, false)]),
        );

        assert_eq!(
            released.action,
            Some(PlaylistAction::ContinueImport(preview.preview_id()))
        );
    }

    #[test]
    fn cancel_and_continue_are_keyboard_focusable_with_space_and_enter() {
        for (tab_count, activation, expected) in [
            (
                1,
                Key::Space,
                PlaylistAction::CancelImport(
                    preview_with_issues(PlaylistImportIntent::AppendToQueue).preview_id(),
                ),
            ),
            (
                2,
                Key::Enter,
                PlaylistAction::ContinueImport(
                    preview_with_issues(PlaylistImportIntent::AppendToQueue).preview_id(),
                ),
            ),
        ] {
            let context = Context::default();
            let preview = preview_with_issues(PlaylistImportIntent::AppendToQueue);
            let _ = frame(&context, &preview, raw_input(Vec::new()));
            let mut focused = frame(&context, &preview, keyboard_input(Key::Tab));
            if tab_count == 2 {
                focused = frame(&context, &preview, keyboard_input(Key::Tab));
            }
            assert!(focused.cancel_has_focus || focused.continue_has_focus);
            let activated = frame(&context, &preview, keyboard_input(activation));
            assert_eq!(activated.action, Some(expected));
        }
    }

    #[test]
    fn both_preview_buttons_keep_accessibility_sized_hit_areas() {
        let context = Context::default();
        let preview = preview_with_issues(PlaylistImportIntent::AppendToQueue);
        let result = frame(&context, &preview, raw_input(Vec::new()));

        assert!(result.cancel_rect.width() > 0.0);
        assert!(result.continue_rect.width() > result.cancel_rect.width());
    }
}
