//! Маршрутизация действий окна настроек.

use super::support::*;

#[test]
fn connect_action_is_intercepted_runs_and_does_not_reach_the_settings_runtime() {
    let world = World::new();
    world.install_browser("chromium");
    let mut integration = world.integration(FakeFlatpak::default(), OfferSetting::Disabled);
    wait_until_idle(&mut integration);
    let mut actions = vec![
        SettingsUiAction::BrowserIntegration {
            target_id: "chromium".to_owned(),
            command: BrowserCommand::Connect,
        },
        SettingsUiAction::Apply,
    ];

    let effect = route_settings_actions(&mut integration, &mut actions);

    assert_eq!(effect, RoutingEffect::VisibleChange);
    // Остался только чужой Apply: ни Cancel, ни закрытия окна настроек не появилось.
    assert_eq!(actions, [SettingsUiAction::Apply]);
    wait_until_idle(&mut integration);
    assert!(
        world.chromium_manifest().is_file(),
        "the click had a real effect"
    );
}

#[test]
fn opening_settings_requests_a_fresh_survey() {
    let world = World::new();
    world.install_browser("chromium");
    let mut integration = world.integration(FakeFlatpak::default(), OfferSetting::Disabled);
    wait_until_idle(&mut integration);
    // Браузер появился после старта: новый обзор должен его увидеть.
    world.install_browser("brave");
    assert!(
        integration
            .panel_view()
            .rows
            .iter()
            .all(|row| row.name != "Brave")
    );

    let mut actions = vec![SettingsUiAction::Open];
    route_settings_actions(&mut integration, &mut actions);
    wait_until_idle(&mut integration);

    assert_eq!(
        actions,
        [SettingsUiAction::Open],
        "Open still reaches the runtime"
    );
    assert!(
        integration
            .panel_view()
            .rows
            .iter()
            .any(|row| row.name == "Brave")
    );
}

#[test]
fn set_up_answer_turns_into_open_settings_action() {
    let world = World::new();
    world.install_browser("chromium");
    let (mut integration, toast_id) = shown_offer(&world);
    integration.answer_offer(toast_id, OfferAnswer::SetUp);
    let mut actions = Vec::new();

    let effect = route_settings_actions(&mut integration, &mut actions);

    assert_eq!(effect, RoutingEffect::SettingsPanelRequested);
    assert_eq!(actions, [SettingsUiAction::Open]);
    // Просьба одноразовая: следующий кадр настроек не открывает.
    let mut next_frame = Vec::new();
    assert_eq!(
        route_settings_actions(&mut integration, &mut next_frame),
        RoutingEffect::Nothing
    );
    assert!(next_frame.is_empty());
}
