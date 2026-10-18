//! Web-версия звука из меню «♪» идёт тем же production bridge, что строка URL sidebar:
//! позиция и Pause сохраняются, отказ до barrier-а оставляет текущий звук и selector.

use super::*;
use crate::state::same_item_candidate_switch::ComponentSwitchOrigin;
use crate::web_media_stream_model::component_variants::{
    ComponentVariantSelectionAction, WebMediaComponentVariantAxisKind,
};

/// Выбор E-AC-3 (вторая строка audio axis) из меню аудиодорожек.
fn eac3_menu_start(
    item_id: PlaylistItemId,
    media_instance_id: MediaInstanceId,
    parent_generation: WebMediaStreamGeneration,
    state: PlaybackState,
) -> (SameItemSwitchAppStart, ComponentVariantSelectionAction) {
    let action = ComponentVariantSelectionAction {
        parent_generation,
        catalog_generation: web_media_core::ComponentVariantCatalogGeneration::new(4),
        axis: WebMediaComponentVariantAxisKind::Audio,
        variant_index: 1,
    };
    let mut start = app_start(item_id, media_instance_id, parent_generation, state);
    start.kind = SameItemSwitchKind::Component {
        action,
        origin: ComponentSwitchOrigin::AudioTrackMenu,
    };
    (start, action)
}

fn menu_context(
    poll_steps: VecDeque<FakePollStep>,
    item_id: PlaylistItemId,
    playback: PlaybackObservation,
    generation: WebMediaStreamGeneration,
) -> FakeSameItemSwitchContext {
    FakeSameItemSwitchContext {
        request_id: media_open_request_id(91),
        poll_steps,
        begin_expected_active: None,
        begin_playback_intent: None,
        controller: crate::web_media_stream_model::UrlSidebarController::default(),
        item_id,
        source_lineage: 92,
        visible_generation: generation,
        playback,
        remembered_targets: Vec::new(),
        remembered_audio_languages: Vec::new(),
        terminal_selector_errors: Vec::new(),
        fallback_notice_visible: true,
    }
}

#[test]
fn audio_menu_version_switch_keeps_paused_position_and_installs_new_version() {
    let item_id = playlist_item_id(93);
    let old_instance = media_instance_id(94);
    let new_instance = media_instance_id(95);
    let old_generation = WebMediaStreamGeneration::for_test(92, 1);
    let new_generation = WebMediaStreamGeneration::for_test(92, 2);
    let old_playback = PlaybackObservation {
        media_instance_id: old_instance,
        position: Duration::from_millis(37_250),
        state: PlaybackState::Paused,
    };
    let mut context = menu_context(
        VecDeque::from([
            FakePollStep::Pending,
            FakePollStep::Installed {
                media_instance_id: new_instance,
                generation: new_generation,
            },
        ]),
        item_id,
        old_playback,
        old_generation,
    );
    let (start, action) =
        eac3_menu_start(item_id, old_instance, old_generation, PlaybackState::Paused);
    let mut path = test_app_path();

    assert_eq!(
        path.start(start, &mut context)
            .expect("audio menu version switch starts"),
        UrlSidebarActionApplyOutcome::Started
    );
    let pending = path.pending.as_ref().expect("switch is pending");
    assert_eq!(
        pending.component_origin(),
        Some(ComponentSwitchOrigin::AudioTrackMenu)
    );
    assert_eq!(
        pending.kind.pending_selection(),
        UrlSidebarPendingSelection::Component(action),
        "the sidebar shows the same pending audio row"
    );
    assert_eq!(
        context.begin_playback_intent,
        Some(PlaybackIntent::StartPaused)
    );
    assert_eq!(
        context.playback, old_playback,
        "old audio plays until Installed"
    );

    assert_eq!(path.poll(&mut context), SameItemSwitchAppPoll::Pending);
    assert_eq!(context.playback, old_playback);
    assert_eq!(path.poll(&mut context), SameItemSwitchAppPoll::Installed);
    assert_eq!(context.playback.media_instance_id, new_instance);
    assert_eq!(context.playback.position, old_playback.position);
    assert_eq!(context.playback.state, PlaybackState::Paused);
    assert!(
        context.remembered_targets.is_empty(),
        "a component switch never becomes a picker preference"
    );
    assert!(
        context.fallback_notice_visible,
        "component switch does not own the fallback notice"
    );
    assert!(context.terminal_selector_errors.is_empty());
}

#[test]
fn audio_menu_version_switch_failure_keeps_current_audio_and_reports_safe_error() {
    let item_id = playlist_item_id(96);
    let old_instance = media_instance_id(97);
    let old_generation = WebMediaStreamGeneration::for_test(92, 5);
    let old_playback = PlaybackObservation {
        media_instance_id: old_instance,
        position: Duration::from_secs(12),
        state: PlaybackState::Playing,
    };
    let mut context = menu_context(
        VecDeque::from([FakePollStep::PreBarrierFailure]),
        item_id,
        old_playback,
        old_generation,
    );
    let (start, _) = eac3_menu_start(
        item_id,
        old_instance,
        old_generation,
        PlaybackState::Playing,
    );
    let mut path = test_app_path();

    path.start(start, &mut context)
        .expect("audio menu version switch starts");
    assert_eq!(
        context.begin_playback_intent,
        Some(PlaybackIntent::StartPlaying)
    );
    assert_eq!(
        path.poll(&mut context),
        SameItemSwitchAppPoll::Failed(UrlSidebarSafeError::SourceUnavailable),
        "AppState turns this outcome of an AudioTrackMenu switch into a toast"
    );
    assert!(path.pending.is_none());
    assert_eq!(
        context.playback, old_playback,
        "current audio keeps playing"
    );
    assert_eq!(
        context.terminal_selector_errors,
        vec![(old_generation, UrlSidebarSafeError::SourceUnavailable)]
    );
}

/// Выбор пользователя «запомнить как `tag`».
fn user_choice(tag: &str) -> AudioLanguagePurpose {
    AudioLanguagePurpose::UserChoice {
        remember_as: Some(language(tag)),
    }
}

fn language(tag: &str) -> playlist_state::RememberedWebAudioLanguage {
    playlist_state::RememberedWebAudioLanguage::new(tag.to_owned()).expect("language tag")
}

/// Смена языка звука (дубляж) идёт тем же production bridge: позиция и Pause сохраняются,
/// старый звук живёт до Installed, а отказ уходит в toast меню «♪».
#[test]
fn audio_language_switch_keeps_paused_position_and_installs_new_language() {
    let item_id = playlist_item_id(98);
    let old_instance = media_instance_id(99);
    let new_instance = media_instance_id(100);
    let old_generation = WebMediaStreamGeneration::for_test(92, 7);
    let new_generation = WebMediaStreamGeneration::for_test(92, 8);
    let old_playback = PlaybackObservation {
        media_instance_id: old_instance,
        position: Duration::from_millis(61_500),
        state: PlaybackState::Paused,
    };
    let mut context = menu_context(
        VecDeque::from([
            FakePollStep::Pending,
            FakePollStep::Installed {
                media_instance_id: new_instance,
                generation: new_generation,
            },
        ]),
        item_id,
        old_playback,
        old_generation,
    );
    let mut start = app_start(item_id, old_instance, old_generation, PlaybackState::Paused);
    start.kind = SameItemSwitchKind::AudioLanguage {
        parent_generation: old_generation,
        purpose: user_choice("ru"),
    };
    let mut path = test_app_path();

    assert_eq!(
        path.start(start, &mut context)
            .expect("audio language switch starts"),
        UrlSidebarActionApplyOutcome::Started
    );
    let pending = path.pending.as_ref().expect("switch is pending");
    assert_eq!(
        pending.component_origin(),
        Some(ComponentSwitchOrigin::AudioTrackMenu),
        "a failed language switch must reach the audio menu toast"
    );
    assert_eq!(
        context.begin_playback_intent,
        Some(PlaybackIntent::StartPaused)
    );
    assert_eq!(
        context.playback, old_playback,
        "old language plays until Installed"
    );

    assert_eq!(path.poll(&mut context), SameItemSwitchAppPoll::Pending);
    assert_eq!(path.poll(&mut context), SameItemSwitchAppPoll::Installed);
    assert_eq!(context.playback.media_instance_id, new_instance);
    assert_eq!(context.playback.position, old_playback.position);
    assert_eq!(context.playback.state, PlaybackState::Paused);
    assert!(
        context.remembered_targets.is_empty(),
        "a language switch never becomes a picker preference"
    );
    assert_eq!(
        context.remembered_audio_languages,
        vec![language("ru")],
        "the language is remembered once the switch has installed"
    );
}

#[test]
fn audio_language_switch_failure_keeps_current_language_and_reports_safe_error() {
    let item_id = playlist_item_id(101);
    let old_instance = media_instance_id(102);
    let old_generation = WebMediaStreamGeneration::for_test(92, 9);
    let old_playback = PlaybackObservation {
        media_instance_id: old_instance,
        position: Duration::from_secs(5),
        state: PlaybackState::Playing,
    };
    let mut context = menu_context(
        VecDeque::from([FakePollStep::PreBarrierFailure]),
        item_id,
        old_playback,
        old_generation,
    );
    let mut start = app_start(
        item_id,
        old_instance,
        old_generation,
        PlaybackState::Playing,
    );
    start.kind = SameItemSwitchKind::AudioLanguage {
        parent_generation: old_generation,
        purpose: user_choice("ru"),
    };
    let mut path = test_app_path();

    path.start(start, &mut context)
        .expect("audio language switch starts");
    assert_eq!(
        path.poll(&mut context),
        SameItemSwitchAppPoll::Failed(UrlSidebarSafeError::SourceUnavailable)
    );
    assert!(path.pending.is_none());
    assert_eq!(
        context.playback, old_playback,
        "current language keeps playing"
    );
}
