//! Исход выбора зрителя на странице (протокол 1.8, сессия 15c2) сквозь настоящую медиа-часть:
//! «Switching…» вместо мгновенного «Audio: …», тост по исходу и судьба возврата секунды.
//!
//! Возврат проверяется поведением, а не флагом: после отказа следующее поколение страницы
//! НЕ уходит на секунду зрителя, после `done` — уходит (см. `assert_no_carry_after`).

use browser_bridge_protocol::ProtocolVersion;
use browser_bridge_protocol::session::{
    NextEpisodeAvailability, VariantKind, VariantOutcome, VariantRequest,
};
use browser_bridge_protocol::text::VariantId;

use super::generation_placement::page_seek_targets;
use super::page_variants::{
    VIEWER_SECOND, assert_no_carry_after, commands_to_page, current_choice, menu_notice,
    navigation_notice, page_opens_generation_2, viewer_selects_dub, viewer_selects_subtitle_line,
    watching_harness,
};
use super::*;
use crate::browser_session::page_variants::{PageVariantOutcomeWord, PageVariantUpdate};
use crate::browser_session::session_media::page_variant_outcome::PAGE_VARIANT_OUTCOME_LINE;
use crate::browser_session::session_media::page_variants::NextEpisodeRequestOutcome;
use crate::browser_session::test_log_capture::with_captured_log;
use crate::page_variants::{
    PageVariantKind, PageVariantNotice, PageVariantRefusal, PageVariantSubject,
};
use crate::viewer_transport::ViewerTransportIntent;

fn id(text: &str) -> VariantId {
    VariantId::new(text).expect("variant id")
}

fn outcome_notice(
    session: &SessionRef,
    request: VariantRequest,
    outcome: VariantOutcome,
) -> IntakeNotice {
    IntakeNotice::PageVariants {
        session: session.clone(),
        update: PageVariantUpdate::Outcome(PageVariantOutcomeWord { request, outcome }),
    }
}

fn select_request(menu: VariantKind, variant: &str) -> VariantRequest {
    VariantRequest::SelectVariant {
        menu,
        variant_id: id(variant),
    }
}

/// Страница ответила исходом `outcome` на просьбу `request`; возвращает журнал шага.
fn page_answers(
    harness: &mut Harness,
    current: &SessionRef,
    request: VariantRequest,
    outcome: VariantOutcome,
) -> String {
    let ((), logs) = with_captured_log(|| {
        harness
            .media
            .on_intake(vec![outcome_notice(current, request, outcome)], None);
    });
    logs
}

/// 1.8, меню озвучек и качеств есть; зритель ещё ничего не выбирал.
fn watching_1_8_with_menus() -> (Harness, SessionRef, PushedStreamWriter) {
    let (mut harness, current, writer) = watching_harness(ProtocolVersion::V1_8);
    harness.media.on_intake(
        vec![
            menu_notice(&current, VariantKind::Audio, &["orig", "dub"]),
            menu_notice(&current, VariantKind::Quality, &["1080", "720"]),
            navigation_notice(&current, NextEpisodeAvailability::Available),
        ],
        None,
    );
    (harness, current, writer)
}

fn viewer_selects_quality_720(harness: &mut Harness) {
    let choice = current_choice(harness, PageVariantKind::Quality, 1);
    harness.media.on_viewer_transport(
        ViewerTransportIntent::SelectPageVariant(choice),
        Instant::now(),
    );
    harness.media.drive(Instant::now());
}

fn selected(kind: PageVariantKind, label: &str) -> Option<PageVariantNotice> {
    Some(PageVariantNotice::Selected {
        kind,
        label: label.into(),
    })
}

fn refused(menu: PageVariantKind, reason: PageVariantRefusal) -> Option<PageVariantNotice> {
    Some(PageVariantNotice::Refused {
        subject: PageVariantSubject::Menu(menu),
        reason,
    })
}

// ---- Switching до исхода -------------------------------------------------------------

#[test]
fn with_1_7_the_label_is_shown_at_once_and_a_stray_outcome_changes_nothing() {
    let (mut harness, current, _writer) = watching_harness(ProtocolVersion::V1_7);
    harness.media.on_intake(
        vec![menu_notice(&current, VariantKind::Audio, &["orig", "dub"])],
        None,
    );
    viewer_selects_dub(&mut harness);
    assert_eq!(
        harness.media.take_page_variant_notice(),
        selected(PageVariantKind::Audio, "dub"),
        "protocol 1.7 keeps the immediate text"
    );
    // Сторож протокола такое сообщение не пропустит; если бы дошло, плеер его не ждал.
    let logs = page_answers(
        &mut harness,
        &current,
        select_request(VariantKind::Audio, "dub"),
        VariantOutcome::Failed,
    );
    assert!(logs.contains("matched=false"), "{logs}");
    assert_eq!(harness.media.take_page_variant_notice(), None);
}

#[test]
fn with_1_8_the_viewer_hears_switching_first_and_the_journal_has_no_label() {
    let (mut harness, current, _writer) = watching_1_8_with_menus();
    viewer_selects_dub(&mut harness);
    assert_eq!(
        commands_to_page(&mut harness),
        vec![super::page_variants::select(VariantKind::Audio, "dub")],
        "the request itself is the same as before"
    );
    assert_eq!(
        harness.media.take_page_variant_notice(),
        Some(PageVariantNotice::Switching {
            kind: PageVariantKind::Audio
        })
    );
    viewer_selects_quality_720(&mut harness);
    assert_eq!(
        harness.media.take_page_variant_notice(),
        Some(PageVariantNotice::Switching {
            kind: PageVariantKind::Quality
        })
    );
    let _keep_current_alive = current;
}

// ---- done ----------------------------------------------------------------------------

#[test]
fn done_confirms_audio_with_the_label_keeps_the_carry_and_leaves_the_menu_alone() {
    let (mut harness, current, _writer) = watching_1_8_with_menus();
    viewer_selects_dub(&mut harness);
    harness.media.take_page_variant_notice();
    let menu_before = harness.media.page_variant_facts();

    let logs = page_answers(
        &mut harness,
        &current,
        select_request(VariantKind::Audio, "dub"),
        VariantOutcome::Done,
    );
    assert!(
        logs.contains(&format!(
            "{PAGE_VARIANT_OUTCOME_LINE} kind=audio outcome=done matched=true"
        )),
        "{logs}"
    );
    assert!(!logs.contains("dub\""), "no label in the journal: {logs}");
    assert_eq!(
        harness.media.take_page_variant_notice(),
        selected(PageVariantKind::Audio, "dub")
    );
    assert_eq!(
        harness.media.page_variant_facts(),
        menu_before,
        "the checkmark follows the page menu only, the outcome does not move it"
    );

    // Возврат секунды жив: следующее поколение уходит на секунду зрителя.
    commands_to_page(&mut harness);
    let _second = page_opens_generation_2(&mut harness, &current);
    harness.media.drive(Instant::now());
    assert_eq!(
        page_seek_targets(&harness.page.take_sent()),
        vec![VIEWER_SECOND]
    );
}

#[test]
fn done_confirms_quality_with_its_label() {
    let (mut harness, current, _writer) = watching_1_8_with_menus();
    viewer_selects_quality_720(&mut harness);
    harness.media.take_page_variant_notice();
    page_answers(
        &mut harness,
        &current,
        select_request(VariantKind::Quality, "720"),
        VariantOutcome::Done,
    );
    assert_eq!(
        harness.media.take_page_variant_notice(),
        selected(PageVariantKind::Quality, "720")
    );
}

// ---- отказы --------------------------------------------------------------------------

#[test]
fn every_refusal_of_an_audio_pick_gives_its_reason_and_cancels_the_carry() {
    for (outcome, reason) in [
        (
            VariantOutcome::NoSuchEpisode,
            PageVariantRefusal::NoSuchEpisode,
        ),
        (VariantOutcome::PersonOnly, PageVariantRefusal::PersonOnly),
        (VariantOutcome::Failed, PageVariantRefusal::Failed),
    ] {
        let (mut harness, current, _writer) = watching_1_8_with_menus();
        viewer_selects_dub(&mut harness);
        harness.media.take_page_variant_notice();
        let logs = page_answers(
            &mut harness,
            &current,
            select_request(VariantKind::Audio, "dub"),
            outcome,
        );
        assert!(logs.contains("matched=true"), "{outcome:?}: {logs}");
        assert!(
            logs.contains("PageRefusedSelection"),
            "{outcome:?}: the carry end is journaled: {logs}"
        );
        assert_eq!(
            harness.media.take_page_variant_notice(),
            refused(PageVariantKind::Audio, reason),
            "{outcome:?}"
        );
        assert_no_carry_after(&mut harness, &current);
    }
}

#[test]
fn a_quality_refusal_gives_its_reason_and_cancels_the_carry() {
    let (mut harness, current, _writer) = watching_1_8_with_menus();
    viewer_selects_quality_720(&mut harness);
    harness.media.take_page_variant_notice();
    page_answers(
        &mut harness,
        &current,
        select_request(VariantKind::Quality, "720"),
        VariantOutcome::PersonOnly,
    );
    assert_eq!(
        harness.media.take_page_variant_notice(),
        refused(PageVariantKind::Quality, PageVariantRefusal::PersonOnly)
    );
    assert_no_carry_after(&mut harness, &current);
}

// ---- чужие и устаревшие исходы -------------------------------------------------------

#[test]
fn an_outcome_for_a_different_variant_or_kind_or_a_repeat_changes_nothing() {
    let (mut harness, current, _writer) = watching_1_8_with_menus();
    viewer_selects_dub(&mut harness);
    harness.media.take_page_variant_notice();
    for request in [
        select_request(VariantKind::Audio, "orig"),
        select_request(VariantKind::Quality, "dub"),
        VariantRequest::NextEpisode {},
    ] {
        let logs = page_answers(&mut harness, &current, request, VariantOutcome::Failed);
        assert!(logs.contains("matched=false"), "{logs}");
        assert_eq!(harness.media.take_page_variant_notice(), None);
    }
    // Ожидание цело: настоящий исход всё ещё срабатывает ровно один раз.
    page_answers(
        &mut harness,
        &current,
        select_request(VariantKind::Audio, "dub"),
        VariantOutcome::Done,
    );
    assert_eq!(
        harness.media.take_page_variant_notice(),
        selected(PageVariantKind::Audio, "dub")
    );
    let logs = page_answers(
        &mut harness,
        &current,
        select_request(VariantKind::Audio, "dub"),
        VariantOutcome::Failed,
    );
    assert!(logs.contains("matched=false"), "{logs}");
    assert_eq!(harness.media.take_page_variant_notice(), None);
    // Чужой отказ не снял возврат секунды.
    commands_to_page(&mut harness);
    let _second = page_opens_generation_2(&mut harness, &current);
    harness.media.drive(Instant::now());
    assert_eq!(
        page_seek_targets(&harness.page.take_sent()),
        vec![VIEWER_SECOND]
    );
}

#[test]
fn a_failed_older_selection_does_not_kill_the_carry_of_a_newer_one() {
    let (mut harness, current, _writer) = watching_1_8_with_menus();
    viewer_selects_dub(&mut harness);
    viewer_selects_quality_720(&mut harness);
    commands_to_page(&mut harness);
    // Озвучка отказала, но возврат теперь принадлежит выбору качества.
    page_answers(
        &mut harness,
        &current,
        select_request(VariantKind::Audio, "dub"),
        VariantOutcome::NoSuchEpisode,
    );
    harness.media.take_page_variant_notice();
    let _second = page_opens_generation_2(&mut harness, &current);
    harness.media.drive(Instant::now());
    assert_eq!(
        page_seek_targets(&harness.page.take_sent()),
        vec![VIEWER_SECOND],
        "the quality pick still returns to the viewer second"
    );
}

#[test]
fn a_new_pick_of_the_same_kind_makes_the_answer_to_the_older_one_stale() {
    let (mut harness, current, _writer) = watching_1_8_with_menus();
    viewer_selects_dub(&mut harness);
    // Зритель передумал: выбрал «orig» (пункт 0) того же меню.
    let choice = current_choice(&harness, PageVariantKind::Audio, 0);
    harness.media.on_viewer_transport(
        ViewerTransportIntent::SelectPageVariant(choice),
        Instant::now(),
    );
    harness.media.take_page_variant_notice();
    let logs = page_answers(
        &mut harness,
        &current,
        select_request(VariantKind::Audio, "dub"),
        VariantOutcome::Failed,
    );
    assert!(logs.contains("matched=false"), "{logs}");
    assert_eq!(harness.media.take_page_variant_notice(), None);
}

// ---- Next и субтитры -----------------------------------------------------------------

#[test]
fn a_next_episode_refusal_is_told_and_done_adds_nothing() {
    let (mut harness, current, _writer) = watching_1_8_with_menus();
    assert_eq!(
        harness.media.viewer_asks_next_episode(Instant::now()),
        NextEpisodeRequestOutcome::Queued
    );
    assert_eq!(
        harness.media.take_page_variant_notice(),
        Some(PageVariantNotice::NextEpisodeRequested)
    );
    page_answers(
        &mut harness,
        &current,
        VariantRequest::NextEpisode {},
        VariantOutcome::Failed,
    );
    assert_eq!(
        harness.media.take_page_variant_notice(),
        Some(PageVariantNotice::Refused {
            subject: PageVariantSubject::NextEpisode,
            reason: PageVariantRefusal::Failed,
        })
    );

    assert_eq!(
        harness.media.viewer_asks_next_episode(Instant::now()),
        NextEpisodeRequestOutcome::Queued
    );
    harness.media.take_page_variant_notice();
    let logs = page_answers(
        &mut harness,
        &current,
        VariantRequest::NextEpisode {},
        VariantOutcome::Done,
    );
    assert!(
        logs.contains("kind=next_episode outcome=done matched=true"),
        "{logs}"
    );
    assert_eq!(harness.media.take_page_variant_notice(), None);
}

#[test]
fn a_subtitle_outcome_is_only_a_journal_line() {
    let (mut harness, current, _writer) = watching_harness(ProtocolVersion::V1_8);
    harness.media.on_intake(
        vec![menu_notice(
            &current,
            VariantKind::Subtitles,
            &["off", "uk"],
        )],
        None,
    );
    viewer_selects_subtitle_line(&mut harness, 1);
    assert_eq!(harness.media.take_page_variant_notice(), None);
    let logs = page_answers(
        &mut harness,
        &current,
        select_request(VariantKind::Subtitles, "uk"),
        VariantOutcome::Failed,
    );
    assert!(
        logs.contains(&format!(
            "{PAGE_VARIANT_OUTCOME_LINE} kind=subtitles outcome=failed matched=true"
        )),
        "{logs}"
    );
    assert_eq!(harness.media.take_page_variant_notice(), None);
}

// ---- новый сеанс ---------------------------------------------------------------------

#[test]
fn a_new_session_forgets_the_pending_selection() {
    let (mut harness, current, _writer) = watching_1_8_with_menus();
    viewer_selects_dub(&mut harness);
    harness.media.take_page_variant_notice();
    let next = session(2);
    harness.media.on_session_opened(&next);
    harness
        .media
        .on_session_attached(&next, ProtocolVersion::V1_8);
    let logs = page_answers(
        &mut harness,
        &next,
        select_request(VariantKind::Audio, "dub"),
        VariantOutcome::Failed,
    );
    assert!(logs.contains("matched=false"), "{logs}");
    assert_eq!(harness.media.take_page_variant_notice(), None);
    let _old = current;
}
