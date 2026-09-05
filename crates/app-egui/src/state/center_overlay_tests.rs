mod center_overlay_tests {
    use super::*;
    use crate::app_wake::{AppWakeOwner, AppWakePort};
    use crate::playlist_runtime::{
        InAppQueueReplacementAdmission, InAppQueueReplacementIntent, PendingPlaylistConfirmation,
        PlaylistImportIntent, PlaylistImportPreview, PlaylistImportPreviewUiAcceptedFixture,
        PlaylistImportPreviewUiFixture, PlaylistRuntime, UrlAppendActionOutcome,
    };
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

    fn render_overlay(
        snapshot: &PlayerSnapshot,
        error: Option<&str>,
        pending: Option<&str>,
        preview: Option<&PlaylistImportPreview>,
        confirmation: Option<&PendingPlaylistConfirmation>,
    ) -> Vec<String> {
        let context = egui::Context::default();
        let mut painted_text = Vec::new();
        // Два настоящих кадра egui проверяют и initial layout, и повторную отрисовку.
        for _ in 0..2 {
            let mut playlist_output = PlaylistUiOutput::default();
            let output = context.run_ui(
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
                            error,
                            pending,
                            preview,
                            confirmation,
                            &mut playlist_output,
                        )
                        .is_none()
                    );
                },
            );
            assert!(playlist_output.take_actions().is_empty());
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
            assert_eq!(
                render_overlay(
                    &snapshot,
                    Some("Playback failed"),
                    Some("Opening media"),
                    None,
                    None
                ),
                ["Playback failed"]
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
}
