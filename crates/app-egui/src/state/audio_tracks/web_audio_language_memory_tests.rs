//! Память языка звука: что запоминается, когда восстанавливается, почему нет циклов.

use media_source_open::web_media_open::audio_languages::{
    WebAudioLanguageAction, WebAudioLanguageChoice,
};
use playlist_state::RememberedWebAudioLanguage;
use web_media_core::LanguageTag;

use super::web_audio_language_memory::{
    LanguageRestorePlan, RESTORE_ATTEMPT_BUDGET, follow_up_after_restore_attempt,
    language_to_remember, plan_language_restore,
};
use super::web_version_switch::WebAudioSwitchRequest;
use crate::state::same_item_candidate_switch::{SameItemSwitchError, UrlSidebarActionApplyOutcome};
use crate::web_media_stream_model::WebMediaStreamGeneration;

fn action(route_index: usize) -> WebAudioLanguageAction {
    WebAudioLanguageAction::for_test(WebMediaStreamGeneration::for_test(7, 1), route_index)
}

fn choice(route_index: usize, tag: &str, is_active: bool) -> WebAudioLanguageChoice {
    WebAudioLanguageChoice {
        action: action(route_index),
        language: LanguageTag::new(tag).expect("tag"),
        is_original: route_index == 0,
        is_active,
    }
}

fn remembered(tag: &str) -> RememberedWebAudioLanguage {
    RememberedWebAudioLanguage::new(tag.to_owned()).expect("language")
}

fn choices_with_original_active() -> Vec<WebAudioLanguageChoice> {
    vec![
        choice(0, "en-US", true),
        choice(1, "ru", false),
        choice(2, "de", false),
    ]
}

#[test]
fn a_remembered_dub_that_is_not_playing_becomes_exactly_one_switch() {
    assert_eq!(
        plan_language_restore(Some(&remembered("ru")), &choices_with_original_active()),
        LanguageRestorePlan::Switch(action(1))
    );
    assert_eq!(
        plan_language_restore(Some(&remembered("RU")), &choices_with_original_active()),
        LanguageRestorePlan::Switch(action(1)),
        "tags compare without regard to case"
    );
}

#[test]
fn nothing_remembered_already_active_and_absent_languages_issue_no_switch() {
    let choices = choices_with_original_active();
    assert_eq!(
        plan_language_restore(None, &choices),
        LanguageRestorePlan::NothingRemembered
    );
    assert_eq!(
        plan_language_restore(Some(&remembered("en-US")), &choices),
        LanguageRestorePlan::AlreadyActive,
        "a remembered original needs no reopen"
    );
    assert_eq!(
        plan_language_restore(Some(&remembered("fr")), &choices),
        LanguageRestorePlan::LanguageAbsent
    );
    assert_eq!(
        plan_language_restore(Some(&remembered("ru")), &[]),
        LanguageRestorePlan::LanguageAbsent,
        "a source without language choices never switches"
    );
}

#[test]
fn the_remembered_value_is_the_tag_of_the_chosen_action_original_included() {
    let choices = choices_with_original_active();
    assert_eq!(
        language_to_remember(&choices, action(1)),
        Some(remembered("ru"))
    );
    assert_eq!(
        language_to_remember(&choices, action(0)),
        Some(remembered("en-US")),
        "going back to the original is remembered too"
    );
    assert_eq!(language_to_remember(&choices, action(9)), None);
}

#[test]
fn restore_is_retried_only_while_the_open_settles_and_never_after_a_failure() {
    let retry = |outcome: &Result<UrlSidebarActionApplyOutcome, SameItemSwitchError>, left| {
        follow_up_after_restore_attempt(outcome, action(1), left)
    };

    assert_eq!(
        retry(&Err(SameItemSwitchError::Busy), RESTORE_ATTEMPT_BUDGET),
        Some(WebAudioSwitchRequest::RestoreRemembered {
            action: action(1),
            attempts_left: RESTORE_ATTEMPT_BUDGET - 1,
        })
    );
    assert_eq!(
        retry(&Err(SameItemSwitchError::Busy), 1),
        None,
        "budget is spent"
    );
    assert_eq!(retry(&Ok(UrlSidebarActionApplyOutcome::Started), 50), None);
    assert_eq!(retry(&Ok(UrlSidebarActionApplyOutcome::NoChange), 50), None);
    assert_eq!(
        retry(&Err(SameItemSwitchError::MissingCapabilities), 50),
        None,
        "a real failure is final: no retry loop"
    );
}
