//! Правило предложения и его жизненный цикл.

use super::support::*;

#[test]
fn offer_rule_table() {
    use crate::browser_integration::offer::OfferSkipReason as Reason;
    use BrowserBridgeStatus as Status;
    use OfferVerdict::{Show, Skip};
    let cases: Vec<(OfferSetting, Vec<Status>, OfferVerdict)> = vec![
        (OfferSetting::Enabled, vec![Status::NotConnected], Show),
        (OfferSetting::Enabled, vec![Status::Outdated], Show),
        (
            OfferSetting::Enabled,
            vec![Status::NotConnected, Status::SnapUnsupported],
            Show,
        ),
        (
            OfferSetting::Disabled,
            vec![Status::NotConnected],
            Skip(Reason::DisabledInSettings),
        ),
        (
            OfferSetting::Enabled,
            vec![Status::NotConnected, Status::Connected],
            Skip(Reason::AlreadyConnected),
        ),
        (
            OfferSetting::Enabled,
            vec![
                Status::NotConnected,
                Status::ConnectedToAnotherCopy {
                    player: PathBuf::from("/x"),
                    player_exists: true,
                },
            ],
            Skip(Reason::AlreadyConnected),
        ),
        (
            OfferSetting::Enabled,
            vec![
                Status::SnapUnsupported,
                Status::FlatpakUnsupported,
                Status::ForeignFile,
            ],
            Skip(Reason::NothingToConnect),
        ),
        (
            OfferSetting::Enabled,
            vec![],
            Skip(Reason::NothingToConnect),
        ),
    ];
    for (setting, statuses, expected) in cases {
        let survey = Ok(survey_with(Ok(()), statuses.clone()));
        assert_eq!(offer_verdict(setting, &survey), expected, "{statuses:?}");
    }
}

#[test]
fn offer_is_skipped_when_survey_failed_or_source_is_unusable() {
    use crate::browser_integration::offer::OfferSkipReason as Reason;
    assert_eq!(
        offer_verdict(
            OfferSetting::Enabled,
            &Err(SurveyError::UnsupportedPlatform)
        ),
        OfferVerdict::Skip(Reason::SurveyFailed)
    );
    let unusable = Ok(survey_with(
        Err(SourceProblem::PlayerExecutableUnknown {
            kind: std::io::ErrorKind::NotFound,
        }),
        vec![BrowserBridgeStatus::NotConnected],
    ));
    assert_eq!(
        offer_verdict(OfferSetting::Enabled, &unusable),
        OfferVerdict::Skip(Reason::SourceUnusable)
    );
}

#[test]
fn startup_survey_yields_one_show_verdict_and_ignores_later_surveys() {
    let world = World::new();
    world.install_browser("chromium");
    let mut integration = world.integration(FakeFlatpak::default(), OfferSetting::Enabled);
    assert_eq!(
        integration.take_startup_offer_verdict(),
        None,
        "no survey yet"
    );
    wait_until_idle(&mut integration);

    assert_eq!(
        integration.take_startup_offer_verdict(),
        Some(OfferVerdict::Show)
    );
    assert_eq!(
        integration.take_startup_offer_verdict(),
        None,
        "delivered once"
    );
    let _ = integration.request_survey();
    wait_until_idle(&mut integration);
    assert_eq!(
        integration.take_startup_offer_verdict(),
        None,
        "later surveys do not re-offer"
    );
}

#[test]
fn no_offer_when_a_browser_is_already_connected() {
    let world = World::new();
    world.install_browser("chromium");
    let mut first = world.integration(FakeFlatpak::default(), OfferSetting::Disabled);
    wait_until_idle(&mut first);
    let _ = first.request_connect(&BrowserTargetId::new("chromium"));
    wait_until_idle(&mut first);

    let mut second = world.integration(FakeFlatpak::default(), OfferSetting::Enabled);
    wait_until_idle(&mut second);
    assert!(matches!(
        second.take_startup_offer_verdict(),
        Some(OfferVerdict::Skip(_))
    ));
}

#[test]
fn set_up_answer_records_dismissal_and_asks_to_open_the_section() {
    let world = World::new();
    world.install_browser("chromium");
    let (mut integration, toast_id) = shown_offer(&world);
    assert_eq!(integration.section_request(), None);

    assert_eq!(
        integration.answer_offer(toast_id, OfferAnswer::SetUp),
        AnswerOutcome::Recorded
    );

    assert_eq!(integration.take_offer_answer(), Some(OfferAnswer::SetUp));
    assert_eq!(integration.take_offer_answer(), None, "recorded once");
    assert_eq!(
        integration.take_settings_open_request(),
        Some(SettingsOpenRequest::BrowserIntegrationSection)
    );
    assert_eq!(integration.take_settings_open_request(), None);
    let request = integration.section_request().expect("section request");
    assert_eq!(request.section, "browser_integration");
    assert_eq!(request.serial, 1);
}

#[test]
fn not_now_and_cross_record_dismissal_without_opening_settings() {
    let world = World::new();
    world.install_browser("chromium");
    for answer in [OfferAnswer::NotNow, OfferAnswer::Closed] {
        let (mut integration, toast_id) = shown_offer(&world);
        assert_eq!(
            integration.answer_offer(toast_id, answer),
            AnswerOutcome::Recorded
        );
        assert_eq!(integration.take_offer_answer(), Some(answer));
        assert_eq!(integration.take_settings_open_request(), None);
        assert_eq!(integration.section_request(), None);
    }
}

#[test]
fn foreign_notification_or_repeated_answer_changes_nothing() {
    let world = World::new();
    world.install_browser("chromium");
    let (mut integration, toast_id) = shown_offer(&world);

    let other = NotificationId::for_tests(99);
    assert_eq!(
        integration.answer_offer(other, OfferAnswer::Closed),
        AnswerOutcome::NotTheOffer
    );
    assert_eq!(integration.take_offer_answer(), None);

    assert_eq!(
        integration.answer_offer(toast_id, OfferAnswer::NotNow),
        AnswerOutcome::Recorded
    );
    assert_eq!(
        integration.answer_offer(toast_id, OfferAnswer::SetUp),
        AnswerOutcome::NotTheOffer,
        "the offer is answered once"
    );
    assert_eq!(integration.take_offer_answer(), Some(OfferAnswer::NotNow));
}
