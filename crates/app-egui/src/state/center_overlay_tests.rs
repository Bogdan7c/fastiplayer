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

/// Что нарисовал один настоящий кадр центрального overlay (сессия 15).
struct OverlayPaint {
    /// Круги и ломаные: так выглядит спиннер (подложка + дуга).
    spinner_shapes: Vec<egui::Shape>,
    /// Весь нарисованный текст.
    text: Vec<String>,
    /// Через сколько egui просит следующий кадр (`Duration::MAX` — не просит).
    repaint_delay: std::time::Duration,
}

impl OverlayPaint {
    /// Центр круглой подложки спиннера, если спиннер нарисован.
    fn spinner_center(&self) -> Option<egui::Pos2> {
        self.spinner_shapes.iter().find_map(|shape| match shape {
            egui::Shape::Circle(circle) => Some(circle.center),
            _ => None,
        })
    }
}

/// Кадр приложения так же, как `AppState`: snapshot → владелец уведомлений → overlay.
fn paint_overlay_frame(
    context: &egui::Context,
    center: &mut NotificationCenter,
    playback_state: PlaybackState,
    motion: UiMotion,
    now: std::time::Instant,
    egui_time_seconds: f64,
) -> OverlayPaint {
    let snapshot = PlayerSnapshot {
        playback_state,
        ..Default::default()
    };
    center.observe_player_snapshot(&snapshot);
    center.observe_playback_waiting(snapshot.playback_state, now);
    let notifications = center.frame(OpenProgress::Idle, motion, now);
    let mut playlist_output = PlaylistUiOutput::default();
    let mut notification_output = NotificationUiOutput::default();
    let output = crate::ui::test_frame::run_ui_frame(
        context,
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 720.0),
            )),
            time: Some(egui_time_seconds),
            ..Default::default()
        },
        |ui| {
            AppState::render_center_overlay(
                ui,
                snapshot.playback_state,
                &notifications,
                &mut notification_output,
                None,
                None,
                &mut playlist_output,
            );
        },
    );
    let mut text = Vec::new();
    let mut spinner_shapes = Vec::new();
    for clipped in output.shapes {
        collect_painted_text(&clipped.shape, &mut text);
        if matches!(clipped.shape, egui::Shape::Circle(_) | egui::Shape::Path(_)) {
            spinner_shapes.push(clipped.shape);
        }
    }
    let repaint_delay = output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map_or(std::time::Duration::MAX, |viewport| viewport.repaint_delay);
    OverlayPaint {
        spinner_shapes,
        text,
        repaint_delay,
    }
}

/// Сессия 15: долгая буферизация рисует спиннер (без текста) в центре видео, короткая —
/// ничего; выход из буферизации убирает спиннер в том же кадре.
#[test]
fn long_buffering_paints_centered_spinner_and_playing_removes_it_same_frame() {
    use crate::state::notifications::BUFFERING_INDICATOR_APPEAR_DELAY;

    let context = egui::Context::default();
    let mut center = NotificationCenter::default();
    let started_at = std::time::Instant::now();
    let early = paint_overlay_frame(
        &context,
        &mut center,
        PlaybackState::Buffering,
        UiMotion::Standard,
        started_at,
        0.0,
    );
    assert!(
        early.spinner_shapes.is_empty(),
        "{:?}",
        early.spinner_shapes
    );

    let stalled_at = started_at + BUFFERING_INDICATOR_APPEAR_DELAY;
    let stalled = paint_overlay_frame(
        &context,
        &mut center,
        PlaybackState::Buffering,
        UiMotion::Standard,
        stalled_at,
        0.5,
    );
    assert_eq!(stalled.spinner_center(), Some(egui::pos2(640.0, 360.0)));
    assert!(
        stalled
            .spinner_shapes
            .iter()
            .any(|shape| matches!(shape, egui::Shape::Path(_)))
    );
    // Решение владельца: только спиннер, без подписи и без подсказки старта.
    assert!(stalled.text.is_empty(), "{:?}", stalled.text);

    let resumed = paint_overlay_frame(
        &context,
        &mut center,
        PlaybackState::Playing,
        UiMotion::Standard,
        stalled_at,
        0.5,
    );
    assert!(
        resumed.spinner_shapes.is_empty(),
        "{:?}",
        resumed.spinner_shapes
    );
}

/// Сессия 15: ошибка в центре важнее спиннера, даже если player всё ещё буферизуется.
#[test]
fn media_failure_is_painted_instead_of_spinner_while_buffering() {
    let context = egui::Context::default();
    let mut center = NotificationCenter::default();
    center.show_media_failure("Playback failed", MediaFailureOrigin::MediaOpen);
    let started_at = std::time::Instant::now();
    paint_overlay_frame(
        &context,
        &mut center,
        PlaybackState::Buffering,
        UiMotion::Standard,
        started_at,
        0.0,
    );
    let painted = paint_overlay_frame(
        &context,
        &mut center,
        PlaybackState::Buffering,
        UiMotion::Standard,
        started_at + std::time::Duration::from_secs(3),
        3.0,
    );
    assert!(painted.text.iter().any(|text| text == "Playback failed"));
    assert_eq!(
        painted.spinner_center(),
        None,
        "{:?}",
        painted.spinner_shapes
    );
}

/// Сессия 15: обычное движение вращает дугу и просит кадры; reduced motion рисует
/// неподвижную дугу и не просит перерисовку ради анимации.
#[test]
fn spinner_rotates_only_with_standard_motion() {
    let long_wait = std::time::Duration::from_secs(2);
    let mut painted_by_motion = Vec::new();
    for motion in [UiMotion::Standard, UiMotion::Reduced] {
        // Контекст с настройками приложения: встроенные анимации egui выключены, поэтому
        // запрос перерисовки может прийти только от спиннера.
        let context = crate::ui::test_frame::app_behavior_context();
        let mut center = NotificationCenter::default();
        let started_at = std::time::Instant::now();
        paint_overlay_frame(
            &context,
            &mut center,
            PlaybackState::Seeking,
            motion,
            started_at,
            0.0,
        );
        let appeared = paint_overlay_frame(
            &context,
            &mut center,
            PlaybackState::Seeking,
            motion,
            started_at + long_wait,
            2.0,
        );
        assert!(appeared.spinner_center().is_some(), "{motion:?}");
        // Свежий контекст сам просит кадры, пока раскладка не устоится: сравниваем
        // устоявшиеся кадры.
        let first = paint_overlay_frame(
            &context,
            &mut center,
            PlaybackState::Seeking,
            motion,
            started_at + long_wait,
            2.15,
        );
        let second = paint_overlay_frame(
            &context,
            &mut center,
            PlaybackState::Seeking,
            motion,
            started_at + long_wait,
            2.3,
        );
        assert!(second.spinner_center().is_some(), "{motion:?}");
        painted_by_motion.push((motion, first, second));
    }

    for (motion, first, second) in painted_by_motion {
        match motion {
            UiMotion::Standard => {
                assert_eq!(first.repaint_delay, std::time::Duration::ZERO);
                assert_ne!(
                    first.spinner_shapes, second.spinner_shapes,
                    "дуга должна вращаться"
                );
            }
            UiMotion::Reduced => {
                assert_eq!(first.repaint_delay, std::time::Duration::MAX);
                assert_eq!(
                    first.spinner_shapes, second.spinner_shapes,
                    "дуга неподвижна"
                );
            }
        }
    }
}
