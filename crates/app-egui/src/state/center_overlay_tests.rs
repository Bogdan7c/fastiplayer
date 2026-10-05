use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::playlist_runtime::{
    InAppQueueReplacementAdmission, InAppQueueReplacementIntent, PendingPlaylistConfirmation,
    PlaylistImportIntent, PlaylistImportPreview, PlaylistImportPreviewUiAcceptedFixture,
    PlaylistImportPreviewUiFixture, PlaylistRuntime, UrlAppendActionOutcome,
};
use crate::state::notifications::{MediaFailureOrigin, NotificationCenter, OpenProgress};
use crate::ui::animation::UiMotion;
use crate::ui::notifications::NotificationUiOutput;
use crate::ui::playlist::PlaylistUiOutput;

const START_HINT: &str = "Open a file or URL to start";

fn collect_painted_text(shape: &egui::Shape, text: &mut Vec<String>) {
    match shape {
        egui::Shape::Text(shape) => text.push(shape.galley.text().to_owned()),
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                collect_painted_text(shape, text);
            }
        }
        _ => {}
    }
}

/// Строит проекцию уведомлений так же, как кадр приложения: ошибка открытия + прогресс.
fn notifications_frame(error: Option<&str>, pending: Option<&str>) -> NotificationsFrame {
    let mut center = NotificationCenter::default();
    if let Some(error) = error {
        center.show_media_failure(error, MediaFailureOrigin::MediaOpen);
    }
    let progress = pending.map_or(OpenProgress::Idle, OpenProgress::InProgress);
    center.frame(progress, UiMotion::Reduced, std::time::Instant::now())
}

fn render_overlay(
    snapshot: &PlayerSnapshot,
    error: Option<&str>,
    pending: Option<&str>,
    preview: Option<&PlaylistImportPreview>,
    confirmation: Option<&PendingPlaylistConfirmation>,
) -> Vec<String> {
    let notifications = notifications_frame(error, pending);
    render_overlay_with_notifications(snapshot, &notifications, preview, confirmation)
}

fn render_overlay_with_notifications(
    snapshot: &PlayerSnapshot,
    notifications: &NotificationsFrame,
    preview: Option<&PlaylistImportPreview>,
    confirmation: Option<&PendingPlaylistConfirmation>,
) -> Vec<String> {
    let context = egui::Context::default();
    let mut painted_text = Vec::new();
    // Настоящие кадры egui проверяют initial layout, измерение toast-области и повторную
    // отрисовку (новая egui `Area` в первом кадре только измеряется).
    for _ in 0..3 {
        let mut playlist_output = PlaylistUiOutput::default();
        let mut notification_output = NotificationUiOutput::default();
        let output = crate::ui::test_frame::run_ui_frame(
            &context,
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 720.0),
                )),
                ..Default::default()
            },
            |ui| {
                assert!(
                    AppState::render_center_overlay(
                        ui,
                        snapshot.playback_state,
                        notifications,
                        &mut notification_output,
                        preview,
                        confirmation,
                        &mut playlist_output,
                    )
                    .is_none()
                );
            },
        );
        assert!(playlist_output.take_actions().is_empty());
        assert!(notification_output.take_actions().is_empty());
        painted_text.clear();
        for clipped in output.shapes {
            collect_painted_text(&clipped.shape, &mut painted_text);
        }
    }
    painted_text
}

#[test]
fn center_overlay_paints_start_hint_only_for_idle_snapshot() {
    for state in [
        PlaybackState::Idle,
        PlaybackState::Opening,
        PlaybackState::Paused,
        PlaybackState::Playing,
        PlaybackState::Buffering,
        PlaybackState::Seeking,
        PlaybackState::Scrubbing,
        PlaybackState::Draining,
        PlaybackState::Ended,
        PlaybackState::Stopped,
        PlaybackState::Failed,
    ] {
        let snapshot = PlayerSnapshot {
            playback_state: state,
            ..Default::default()
        };
        let before = format!("{snapshot:?}");
        let text = render_overlay(&snapshot, None, None, None, None);
        if state == PlaybackState::Idle {
            assert_eq!(text, [START_HINT]);
        } else {
            assert!(text.is_empty(), "{state:?}: {text:?}");
        }
        assert_eq!(format!("{snapshot:?}"), before);
    }
}

#[test]
fn center_overlay_preserves_error_pending_and_queue_priority_without_actions() {
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    runtime.resolve_missing_state_for_test();
    assert!(matches!(
        runtime
            .append_playlist_url(
                "https://media.example.test/queued.mp4",
                &fastiplayer_config::YtDlpConfig::default(),
            )
            .unwrap(),
        UrlAppendActionOutcome::Appended { item_count: 1 }
    ));
    assert!(matches!(
        runtime
            .admit_in_app_queue_replacement(InAppQueueReplacementIntent::local_file(
                "replacement.mp4".into(),
            ))
            .unwrap(),
        InAppQueueReplacementAdmission::AwaitingConfirmation
    ));
    let confirmation = runtime.pending_playlist_confirmation().unwrap();
    let preview = PlaylistImportPreview::for_ui_test(PlaylistImportPreviewUiFixture {
        intent: PlaylistImportIntent::AppendToQueue,
        accepted: PlaylistImportPreviewUiAcceptedFixture {
            singles: 1,
            groups: 0,
            retained_items: 1,
        },
        issue_kinds: &[],
        source_rejected_at_least: None,
        capacity_rejected: None,
        sensitive_durable_locator_count: 0,
    });
    let original_preview = preview.clone();
    let queue_revision = runtime.playlist_view_snapshot().revision();

    for state in [
        PlaybackState::Idle,
        PlaybackState::Paused,
        PlaybackState::Failed,
    ] {
        let snapshot = PlayerSnapshot {
            playback_state: state,
            ..Default::default()
        };
        assert_eq!(
            render_overlay(&snapshot, None, Some("Opening media"), None, None),
            ["Opening media"]
        );
        // Сессия 04: идущее открытие важнее старой ошибки — «Opening media» не прячется.
        assert_eq!(
            render_overlay(
                &snapshot,
                Some("Playback failed"),
                Some("Opening media"),
                None,
                None
            ),
            ["Opening media"]
        );
        // Без открытия фатальная ошибка видна вместе с кнопкой закрытия.
        assert_eq!(
            render_overlay(&snapshot, Some("Playback failed"), None, None, None),
            ["Playback failed", "×"]
        );
        let imported = render_overlay(
            &snapshot,
            Some("Playback failed"),
            Some("Opening media"),
            Some(&preview),
            None,
        );
        assert!(imported.iter().any(|text| text == "Добавить к плейлисту"));
        let confirmed = render_overlay(
            &snapshot,
            Some("Playback failed"),
            Some("Opening media"),
            Some(&preview),
            Some(&confirmation),
        );
        assert!(
            confirmed
                .iter()
                .any(|text| text == "Заменить текущую очередь?")
        );
        assert!(!confirmed.iter().any(|text| text == "Добавить к плейлисту"));
        for text in [&imported, &confirmed] {
            for hidden in [START_HINT, "Playback failed", "Opening media"] {
                assert!(!text.iter().any(|text| text == hidden));
            }
        }
    }
    assert_eq!(preview, original_preview);
    assert_eq!(runtime.pending_playlist_confirmation(), Some(confirmation));
    assert_eq!(runtime.playlist_view_snapshot().revision(), queue_revision);
}

/// Сквозной сценарий сессии 04: настоящий player отклоняет seek без seekable timeline,
/// центральный overlay рисует временный toast, а через 5 с overlay снова пуст.
#[test]
fn real_seek_rejection_is_painted_as_toast_and_overlay_clears_after_lifetime() {
    let mut session = player_core::PlayerSession::new();
    session
        .dispatch_command(PlayerCommand::Seek(player_core::SeekRequest::absolute(
            media_core::MediaTime::from_secs(5),
        )))
        .expect("seek command must be accepted by state machine");
    let started_at = std::time::Instant::now();
    let mut center = NotificationCenter::default();
    for event in session.take_events() {
        center.record_player_event(&event, started_at);
    }
    center.observe_player_snapshot(session.snapshot());
    let snapshot = session.snapshot().clone();

    let visible = render_overlay_with_notifications(
        &snapshot,
        &center.frame(OpenProgress::Idle, UiMotion::Reduced, started_at),
        None,
        None,
    );
    assert!(
        visible.iter().any(|text| text.contains("Seek невозможен")),
        "{visible:?}"
    );
    // Свежий player без media — Idle, поэтому подсказка старта законно остаётся.
    assert_eq!(snapshot.playback_state, PlaybackState::Idle);
    // Старый `last_error` не стал ошибкой в центре: только подсказка, toast и его ×.
    assert_eq!(visible.len(), 3, "{visible:?}");
    assert!(visible.iter().any(|text| text == START_HINT));

    let expired_at = started_at + crate::state::notifications::TRANSIENT_NOTIFICATION_LIFETIME;
    let expired = render_overlay_with_notifications(
        &snapshot,
        &center.frame(OpenProgress::Idle, UiMotion::Reduced, expired_at),
        None,
        None,
    );
    assert_eq!(expired, [START_HINT]);
}
