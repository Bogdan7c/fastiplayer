//! Обмен выбором субтитров со страницей в медиа-части (протокол 1.7, 14a): подсказка страницы
//! ложится на склад субтитров сеанса, выбор зрителя уходит странице `subtitle_choice` — только
//! экстеншену 1.7+ и только пока страница слушает.

use browser_bridge_protocol::ProtocolVersion;
use browser_bridge_protocol::session::{PageSubtitleTrackId, SubtitleChoice};

use super::*;
use crate::browser_session::page_variants::PageVariantUpdate;
use crate::browser_session::session_media::page_subtitles::SubtitleChoiceReportOutcome;
use crate::page_subtitles::{
    PageSubtitlePreference, PageSubtitleTrackNumber, PageSubtitleViewerChoice,
};
use crate::viewer_transport::ViewerTransportIntent;

fn preference_notice(session: &SessionRef, choice: SubtitleChoice) -> IntakeNotice {
    IntakeNotice::PageVariants {
        session: session.clone(),
        update: PageVariantUpdate::SubtitlePreference(choice),
    }
}

fn protocol_track(number: u32) -> SubtitleChoice {
    SubtitleChoice::PageTrack {
        track: PageSubtitleTrackId::new(number).expect("track numbers start at 1"),
    }
}

/// Сообщения `subtitle_choice`, ушедшие странице.
fn subtitle_choices_sent(harness: &mut Harness) -> Vec<SubtitleChoice> {
    harness
        .page
        .take_sent()
        .into_iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::SubtitleChoice { choice } => Some(choice),
            _ => None,
        })
        .collect()
}

fn report(harness: &mut Harness, choice: PageSubtitleViewerChoice) {
    harness.media.on_viewer_transport(
        ViewerTransportIntent::ReportSubtitleChoice(choice),
        Instant::now(),
    );
    harness.media.drive(Instant::now());
}

#[test]
fn the_page_preference_lands_on_the_session_subtitle_shelf_and_the_last_word_wins() {
    let (mut harness, current, _writer) = loaded_harness();
    harness
        .media
        .on_session_attached(&current, ProtocolVersion::V1_7);
    harness.media.on_intake(
        vec![
            preference_notice(&current, protocol_track(4)),
            preference_notice(&current, SubtitleChoice::Off {}),
        ],
        None,
    );
    assert_eq!(
        harness.media.page_subtitle_shelf().preference(),
        PageSubtitlePreference::Off
    );
    // Подсказка — не команда плееру и не сообщение странице.
    assert!(harness.player.take_calls().is_empty());
    assert!(subtitle_choices_sent(&mut harness).is_empty());
}

#[test]
fn a_viewer_choice_reaches_a_1_7_page_once_as_subtitle_choice() {
    let (mut harness, current, _writer) = loaded_harness();
    harness
        .media
        .on_session_attached(&current, ProtocolVersion::V1_7);
    harness.page.take_sent();
    report(
        &mut harness,
        PageSubtitleViewerChoice::PageTrack(PageSubtitleTrackNumber::new(3)),
    );
    assert_eq!(subtitle_choices_sent(&mut harness), vec![protocol_track(3)]);
    report(&mut harness, PageSubtitleViewerChoice::Off);
    assert_eq!(
        subtitle_choices_sent(&mut harness),
        vec![SubtitleChoice::Off {}]
    );
}

#[test]
fn a_1_6_page_or_an_unattached_one_gets_no_subtitle_choice() {
    let (mut harness, current, _writer) = loaded_harness();
    assert_eq!(
        harness
            .media
            .viewer_reports_subtitle_choice(PageSubtitleViewerChoice::Off),
        SubtitleChoiceReportOutcome::NotAttached
    );
    harness
        .media
        .on_session_attached(&current, ProtocolVersion::V1_6);
    assert_eq!(
        harness
            .media
            .viewer_reports_subtitle_choice(PageSubtitleViewerChoice::Off),
        SubtitleChoiceReportOutcome::ProtocolTooOld
    );
    harness.media.drive(Instant::now());
    assert!(subtitle_choices_sent(&mut harness).is_empty());
}
