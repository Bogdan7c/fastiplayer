//! Модель вида панели: тексты, кнопки, ссылки, общие строки.

use super::support::*;

#[test]
fn every_status_maps_to_its_text_and_buttons() {
    use BrowserBridgeStatus as Status;
    let enabled = BrowserButtonAvailability::Enabled;
    let cases: Vec<(Status, &str, Vec<&str>)> = vec![
        (Status::Connected, "Connected", vec!["Disconnect"]),
        (
            Status::ConnectedToAnotherCopy {
                player: PathBuf::from("/opt/other"),
                player_exists: true,
            },
            "Connected to another Fastiplayer copy",
            vec!["Reconnect to this copy", "Disconnect"],
        ),
        (
            Status::ConnectedToAnotherCopy {
                player: PathBuf::from("/opt/gone"),
                player_exists: false,
            },
            "Connected to a Fastiplayer copy that no longer exists",
            vec!["Reconnect to this copy", "Disconnect"],
        ),
        (Status::NotConnected, "Not connected", vec!["Connect"]),
        (
            Status::Outdated,
            "Outdated — reconnect",
            vec!["Reconnect", "Disconnect"],
        ),
        (
            Status::NeedsFlatpakPermission,
            "Needs Flatpak permission",
            vec!["Allow", "Disconnect"],
        ),
        (
            Status::ForeignFile,
            "Another program uses this connection file",
            vec!["Connect"],
        ),
        (
            Status::FlatpakUnsupported,
            "Chromium from Flatpak is not supported yet",
            vec![],
        ),
        (
            Status::SnapUnsupported,
            "Snap browsers are not supported",
            vec![],
        ),
        (
            Status::Unavailable(UnavailableReason::Filesystem {
                path: PathBuf::from("/secret/home/dir"),
                kind: std::io::ErrorKind::PermissionDenied,
            }),
            "Unavailable: cannot read the browser's folder",
            vec![],
        ),
    ];
    for (status, expected_text, expected_buttons) in cases {
        let view = row_view(&report(status.clone()), enabled);
        assert_eq!(view.status_text, expected_text, "{status:?}");
        assert_eq!(button_labels(&view), expected_buttons, "{status:?}");
        assert!(
            !view.status_text.contains("secret"),
            "paths must not leak into the UI"
        );
    }
}

#[test]
fn help_links_belong_to_flatpak_permission_and_snap_only() {
    let enabled = BrowserButtonAvailability::Enabled;
    let flatseal = row_view(
        &report(BrowserBridgeStatus::NeedsFlatpakPermission),
        enabled,
    );
    let link = flatseal.help_link.expect("flatseal link");
    assert_eq!(link.label, "How to allow it in Flatseal");
    assert!(link.url.ends_with("docs/browser-bridge/flatpak.md"));
    let snap = row_view(&report(BrowserBridgeStatus::SnapUnsupported), enabled);
    assert!(
        snap.help_link
            .expect("snap link")
            .url
            .ends_with("README.md#snap-browsers")
    );
    assert_eq!(
        row_view(&report(BrowserBridgeStatus::Connected), enabled).help_link,
        None
    );
}

#[test]
fn row_shows_packaging_and_not_verified_tag() {
    let mut flatpak_report = report(BrowserBridgeStatus::NotConnected);
    flatpak_report.packaging = PackagingKind::Flatpak;
    flatpak_report.verification = Verification::NotVerified;
    let view = row_view(&flatpak_report, BrowserButtonAvailability::Enabled);
    assert_eq!(view.packaging_label, "Flatpak");
    assert!(view.not_verified);
    assert_eq!(view.target_id, "chromium");
}

fn panel_for(survey: Option<&Result<BrowserSurvey, SurveyError>>) -> BrowserPanelView {
    build_panel_view(&PanelInputs {
        survey,
        extensions: None,
        installer_problem: None,
        activity: BrowserPanelActivity::Idle,
        operation_notes: &[],
    })
}

#[test]
fn source_problem_blocks_connect_and_allow_but_keeps_disconnect() {
    let problem = SourceProblem::HostBinaryMissing {
        expected: PathBuf::from("/app/fastiplayer-browser-host"),
        player: PathBuf::from("/app/fastiplayer"),
    };
    let survey = Ok(survey_with(
        Err(problem),
        vec![
            BrowserBridgeStatus::NeedsFlatpakPermission,
            BrowserBridgeStatus::NotConnected,
        ],
    ));
    let view = panel_for(Some(&survey));

    assert_eq!(
        view.global_notes,
        ["This Fastiplayer build has no browser host next to it"]
    );
    let flatpak_row = &view.rows[0];
    let availability: Vec<_> = flatpak_row
        .buttons
        .iter()
        .map(|button| (button.command, button.availability))
        .collect();
    assert_eq!(
        availability,
        [
            (
                BrowserCommand::AllowFlatpakBusAccess,
                BrowserButtonAvailability::Disabled
            ),
            (
                BrowserCommand::Disconnect,
                BrowserButtonAvailability::Enabled
            ),
        ]
    );
    assert_eq!(
        view.rows[1].buttons[0].availability,
        BrowserButtonAvailability::Disabled
    );
}

#[test]
fn unsupported_player_path_and_flatpak_failure_have_their_own_lines() {
    let mut survey = survey_with(
        Err(SourceProblem::PlayerPathUnsupported {
            player: PathBuf::from("/odd'path/fastiplayer"),
            reason: desktop_integration::ActivationServiceFileError::UnsupportedCharacter {
                character: '\'',
            },
        }),
        Vec::new(),
    );
    survey.flatpak_problem = Some(FlatpakError::TimedOut);
    let view = panel_for(Some(&Ok(survey)));
    assert_eq!(
        view.global_notes,
        [
            "Fastiplayer's path contains characters D-Bus cannot start",
            "Flatpak did not answer — Flatpak browsers are not shown",
        ]
    );
}

#[test]
fn empty_survey_says_no_browsers_found_and_missing_survey_says_scanning() {
    let empty = Ok(survey_with(Ok(()), Vec::new()));
    let finished = panel_for(Some(&empty));
    assert_eq!(finished.scan, BrowserScanState::Finished);
    assert_eq!(finished.list_message(), Some("No browsers found"));

    let scanning = panel_for(None);
    assert_eq!(scanning.list_message(), Some("Looking for browsers…"));
}

#[test]
fn working_activity_and_operation_notes_reach_the_view() {
    let survey = Ok(survey_with(Ok(()), vec![BrowserBridgeStatus::Connected]));
    let notes = vec!["Replaced a file that belonged to another program".to_owned()];
    let view = build_panel_view(&PanelInputs {
        survey: Some(&survey),
        extensions: None,
        installer_problem: None,
        activity: BrowserPanelActivity::Working,
        operation_notes: &notes,
    });
    assert_eq!(view.activity, BrowserPanelActivity::Working);
    assert_eq!(view.operation_notes, notes);
    assert_eq!(view.list_message(), None);
}
