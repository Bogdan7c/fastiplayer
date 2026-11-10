//! Склад субтитров страницы в медиа-части: документ текущего сеанса ложится на склад,
//! переживает смену поколения, а новый сеанс начинает пустой склад.

use std::sync::Arc;

use super::*;
use crate::page_subtitles::{
    PageSubtitleDisplay, PageSubtitleDocument, PageSubtitleFormatHint, PageSubtitleLabel,
    PageSubtitleLanguage, PageSubtitleTrackNumber,
};

fn ready(session: &SessionRef, track: u32, bytes: &[u8]) -> IntakeNotice {
    IntakeNotice::PageSubtitleReady {
        session: session.clone(),
        document: Box::new(PageSubtitleDocument {
            track: PageSubtitleTrackNumber::new(track),
            language: PageSubtitleLanguage::Tagged("en".to_owned()),
            label: PageSubtitleLabel::NotGiven,
            format_hint: PageSubtitleFormatHint::WebVtt,
            display: PageSubtitleDisplay::ShowingOnPage,
            source: crate::page_subtitles::PageSubtitleSource::PageDownload,
            bytes: Arc::from(bytes),
        }),
    }
}

fn shelf_tracks(harness: &Harness) -> Vec<(u32, Vec<u8>)> {
    harness
        .media
        .page_subtitle_shelf()
        .entries()
        .iter()
        .map(|entry| (entry.document.track.get(), entry.document.bytes.to_vec()))
        .collect()
}

#[test]
fn a_document_survives_a_new_generation_and_the_player_is_not_touched_by_it() {
    let (mut harness, current, _writer) = loaded_harness();
    harness
        .media
        .on_intake(vec![ready(&current, 1, b"WEBVTT one")], None);
    // Документ не команда плееру и не сообщение странице.
    assert!(harness.player.take_calls().is_empty());
    assert!(harness.page.take_sent().is_empty());

    // Новый `MediaSource` страницы (реклама, смена качества) — новое поколение.
    let (_next_writer, next_generation) = stream_generation(&current, 2, true);
    harness.media.on_intake(vec![next_generation], None);
    harness.drive_until_loaded();
    harness
        .media
        .on_intake(vec![ready(&current, 2, b"WEBVTT two")], None);

    assert_eq!(
        shelf_tracks(&harness),
        vec![(1, b"WEBVTT one".to_vec()), (2, b"WEBVTT two".to_vec())]
    );
}

#[test]
fn a_new_session_starts_an_empty_shelf_and_late_documents_of_the_old_one_are_dropped() {
    let (mut harness, old_session, _writer) = loaded_harness();
    harness
        .media
        .on_intake(vec![ready(&old_session, 1, b"old")], None);
    let old_view = harness.media.page_subtitle_shelf();

    let new_session = session(2);
    harness.media.on_session_opened(&new_session);
    assert!(shelf_tracks(&harness).is_empty());
    assert_ne!(
        harness.media.page_subtitle_shelf().shelf(),
        old_view.shelf()
    );

    // Запоздавший документ прежней вкладки на новый склад не попадает.
    harness
        .media
        .on_intake(vec![ready(&old_session, 2, b"late")], None);
    assert!(shelf_tracks(&harness).is_empty());
    harness
        .media
        .on_intake(vec![ready(&new_session, 1, b"new")], None);
    assert_eq!(shelf_tracks(&harness), vec![(1, b"new".to_vec())]);
}
