//! Тесты семантики Esc: решение по реальному состоянию `PlaylistRuntime` и egui.

use std::path::PathBuf;

use playlist_core::{
    CachedPlaylistMetadata, DurableReopenLocator, LocalLocator, PlaylistImportAvailability,
    PlaylistImportProvenance, PlaylistImportSourceKind, PlaylistMediaKind,
    PlaylistSingleImportDraft,
};

use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::playlist_runtime::{
    InAppQueueReplacementAdmission, InAppQueueReplacementIntent, PlaylistConfirmationApplyOutcome,
    PlaylistImportDraft, PlaylistImportIntent, UrlAppendActionOutcome,
};
use crate::ui::test_frame::{app_behavior_context, run_ui_frame};

/// Runtime с одной строкой в очереди: замена очереди требует подтверждения.
fn runtime_with_queued_item() -> PlaylistRuntime {
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    runtime.resolve_missing_state_for_test();
    let appended = runtime
        .append_playlist_url(
            "https://media.example.test/queued.mp4",
            &fastiplayer_config::YtDlpConfig::default(),
        )
        .expect("тестовый URL добавляется в очередь");
    assert!(matches!(
        appended,
        UrlAppendActionOutcome::Appended { item_count: 1 }
    ));
    runtime
}

/// Открывает диалог «заменить очередь?» тем же путём, что и открытие файла.
fn open_replacement_confirmation(runtime: &mut PlaylistRuntime) {
    let admission = runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::local_file(
            "replacement.mp4".into(),
        ))
        .expect("runtime принимает замену очереди");
    assert!(matches!(
        admission,
        InAppQueueReplacementAdmission::AwaitingConfirmation
    ));
}

/// Публикует preview импорта плейлиста с одной локальной записью.
fn open_import_preview(runtime: &mut PlaylistRuntime) -> PlaylistImportPreviewId {
    open_import_preview_with_intent(runtime, PlaylistImportIntent::AppendToQueue)
}

fn open_import_preview_with_intent(
    runtime: &mut PlaylistRuntime,
    intent: PlaylistImportIntent,
) -> PlaylistImportPreviewId {
    let locator = DurableReopenLocator::local(LocalLocator::Native(PathBuf::from("/imported.mkv")));
    let entry = PlaylistSingleImportDraft::new(
        locator.clone(),
        CachedPlaylistMetadata::new("imported", PlaylistMediaKind::Unknown),
        None,
        Vec::new(),
        PlaylistImportProvenance::new(locator, PlaylistImportSourceKind::M3u, None),
        PlaylistImportAvailability::Available,
    )
    .expect("валидная запись импорта");
    let preview = runtime
        .stage_playlist_import(
            intent,
            PlaylistImportDraft::new(vec![entry.into()], Vec::new(), None, 0),
        )
        .expect("preview импорта публикуется");
    preview.preview_id()
}

fn collect(runtime: &PlaylistRuntime, presentation: WindowPresentation) -> EscapeContext {
    EscapeContext::collect(&app_behavior_context(), runtime, presentation)
}

#[test]
fn escape_without_anything_open_does_nothing_and_keeps_app_alive() {
    let runtime = runtime_with_queued_item();

    let target = resolve_escape_target(&collect(&runtime, WindowPresentation::Windowed));

    // Раньше Esc закрывал приложение; теперь закрывать нечего — ничего и не происходит.
    assert_eq!(target, EscapeTarget::Nothing);
}

#[test]
fn escape_in_fullscreen_exits_fullscreen() {
    let runtime = runtime_with_queued_item();

    let target = resolve_escape_target(&collect(&runtime, WindowPresentation::Fullscreen));

    assert_eq!(target, EscapeTarget::Fullscreen);
}

#[test]
fn escape_cancels_queue_replacement_confirmation_before_fullscreen() {
    let mut runtime = runtime_with_queued_item();
    open_replacement_confirmation(&mut runtime);
    let pending = runtime
        .pending_playlist_confirmation()
        .expect("диалог подтверждения открыт");

    let target = resolve_escape_target(&collect(&runtime, WindowPresentation::Fullscreen));

    let EscapeTarget::PlaylistConfirmation(cancel_action) = target else {
        panic!("Esc должен отменять подтверждение, получено {target:?}");
    };
    assert_eq!(cancel_action.intent_id, pending.intent_id());
    assert_eq!(
        cancel_action.decision,
        QueueReplacementConfirmationDecision::Cancel
    );
    // Применение того же действия, что даёт кнопка «Отмена», закрывает диалог
    // и не трогает очередь.
    let queue_revision = runtime.playlist_view_snapshot().revision();
    let outcome = runtime.respond_to_playlist_confirmation(cancel_action);
    assert!(matches!(
        outcome,
        PlaylistConfirmationApplyOutcome::Cancelled
    ));
    assert_eq!(runtime.pending_playlist_confirmation(), None);
    assert_eq!(runtime.playlist_view_snapshot().revision(), queue_revision);
}

#[test]
fn escape_cancels_import_preview_without_touching_queue() {
    let mut runtime = runtime_with_queued_item();
    let preview_id = open_import_preview(&mut runtime);
    let queue_revision = runtime.playlist_view_snapshot().revision();

    let target = resolve_escape_target(&collect(&runtime, WindowPresentation::Fullscreen));

    assert_eq!(target, EscapeTarget::PlaylistImportPreview(preview_id));
    // Тот же метод, что вызывает `PlaylistAction::CancelImport` от кнопки «Отмена».
    assert!(runtime.cancel_playlist_import(preview_id));
    assert!(runtime.pending_playlist_import_preview().is_none());
    assert_eq!(runtime.playlist_view_snapshot().revision(), queue_revision);
}

#[test]
fn confirmation_has_priority_over_import_preview() {
    let mut runtime = runtime_with_queued_item();
    // Импорт с заменой очереди: «Продолжить» в preview открывает подтверждение
    // поверх ещё не закрытого preview — единственный путь, где они есть вместе.
    let preview_id =
        open_import_preview_with_intent(&mut runtime, PlaylistImportIntent::ReplaceQueue);
    let _continued = runtime.continue_playlist_import(preview_id);
    assert!(
        runtime.pending_playlist_confirmation().is_some(),
        "предусловие: подтверждение замены открыто"
    );
    let context = collect(&runtime, WindowPresentation::Windowed);
    assert!(
        context.playlist_import_preview.is_some(),
        "предусловие: preview импорта тоже открыт"
    );

    let target = resolve_escape_target(&context);

    assert!(matches!(target, EscapeTarget::PlaylistConfirmation(_)));
}

#[test]
fn collecting_escape_context_does_not_change_runtime_state() {
    let mut runtime = runtime_with_queued_item();
    open_replacement_confirmation(&mut runtime);
    let confirmation_before = runtime.pending_playlist_confirmation();
    let queue_revision = runtime.playlist_view_snapshot().revision();

    let _context = collect(&runtime, WindowPresentation::Fullscreen);

    assert_eq!(runtime.pending_playlist_confirmation(), confirmation_before);
    assert_eq!(runtime.playlist_view_snapshot().revision(), queue_revision);
}

fn popup_frame_input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(400.0, 300.0),
        )),
        events,
        ..Default::default()
    }
}

/// Кадр с ComboBox — таким же popup-ом, как выбор facet в URL-сайдбаре.
fn run_combo_box_frame(egui_ctx: &egui::Context, events: Vec<egui::Event>) -> egui::Rect {
    let mut selected_quality = 0_usize;
    let mut combo_rect = egui::Rect::NOTHING;
    let _ = run_ui_frame(egui_ctx, popup_frame_input(events), |ui| {
        let response = egui::ComboBox::from_id_salt("escape_test_combo")
            .selected_text("Качество")
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut selected_quality, 0, "720p");
                ui.selectable_value(&mut selected_quality, 1, "1080p");
            })
            .response;
        combo_rect = response.rect;
    });
    combo_rect
}

#[test]
fn escape_with_open_egui_popup_is_left_to_egui_which_closes_it() {
    let egui_ctx = app_behavior_context();
    let mut runtime = runtime_with_queued_item();
    open_replacement_confirmation(&mut runtime);
    // Открываем ComboBox настоящим кликом мышью.
    let combo_center = run_combo_box_frame(&egui_ctx, Vec::new()).center();
    let pointer_button = |pressed| egui::Event::PointerButton {
        pos: combo_center,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    run_combo_box_frame(&egui_ctx, vec![egui::Event::PointerMoved(combo_center)]);
    run_combo_box_frame(&egui_ctx, vec![pointer_button(true)]);
    run_combo_box_frame(&egui_ctx, vec![pointer_button(false)]);
    run_combo_box_frame(&egui_ctx, Vec::new());
    assert!(
        egui::Popup::is_any_open(&egui_ctx),
        "предусловие: popup открыт"
    );

    let context = EscapeContext::collect(&egui_ctx, &runtime, WindowPresentation::Fullscreen);
    let target = resolve_escape_target(&context);

    // Popup выше диалога и фуллскрина: shell ничего не отменяет сам.
    assert_eq!(target, EscapeTarget::EguiPopup);
    // egui видит тот же Esc в своём кадре и закрывает popup.
    let escape_key = egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    run_combo_box_frame(&egui_ctx, vec![escape_key]);
    run_combo_box_frame(&egui_ctx, Vec::new());
    assert!(!egui::Popup::is_any_open(&egui_ctx));
    // Диалог подтверждения этим нажатием не тронут.
    assert!(runtime.pending_playlist_confirmation().is_some());
}
