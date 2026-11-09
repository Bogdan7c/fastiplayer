//! Субтитры страницы (протокол 1.5) в переходнике: заголовок `subtitle_track_offered` и куски
//! документа собираются в готовый документ, который уходит главному циклу уведомлением;
//! поток видео при этом идёт как без субтитров.

use browser_bridge_protocol::session::{
    PageSubtitleByteLength, PageSubtitleDisplay, PageSubtitleLabel, PageSubtitleLanguage,
    PageSubtitleOrigin, PageSubtitleTrackId, SubtitleFormatHint,
};

use super::*;
use crate::browser_session::stream_queue::OwnedPageSubtitleChunk;

fn track(number: u32) -> PageSubtitleTrackId {
    PageSubtitleTrackId::new(number).expect("valid track")
}

fn offered(instance: u64, number: u32, byte_length: u64) -> SessionInput {
    message(
        instance,
        PageSessionMessage::SubtitleTrackOffered {
            track: track(number),
            language: PageSubtitleLanguage::Unknown {},
            label: PageSubtitleLabel::None {},
            format_hint: SubtitleFormatHint::WebVtt,
            origin: PageSubtitleOrigin::PageDownload,
            page_display: PageSubtitleDisplay::Showing,
            byte_length: PageSubtitleByteLength::new(byte_length).expect("valid length"),
        },
    )
}

fn subtitle_bytes(instance: u64, number: u32, offset: u64, payload: &[u8]) -> SessionInput {
    SessionInput::PageSubtitleBytes(
        session(instance),
        OwnedPageSubtitleChunk {
            track: track(number),
            offset,
            payload: payload.to_vec(),
        },
    )
}

#[test]
fn a_whole_document_reaches_the_main_loop_and_the_video_stream_is_untouched() {
    let (mut translator, log, _failure) = fake_translator();
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            added(1, 1, 0, "video/mp4"),
            offered(1, 1, 6),
            subtitle_bytes(1, 1, 0, b"WEB"),
            bytes(1, 1, 0, b"video"),
            subtitle_bytes(1, 1, 3, b"VTT"),
        ],
    );

    // Поток видео идёт как без субтитров: те же вызовы писателя.
    assert_eq!(
        log.calls(),
        vec![
            (1, WriterCall::AddLane(0, PushedContainer::FragmentedMp4)),
            (1, WriterCall::Append(0, b"video".to_vec())),
        ]
    );
    // Документ отдан один раз и целиком, в порядке провода (после байтов видео).
    assert_eq!(
        seen(&notices),
        vec![
            Seen::NewGeneration(1, 1),
            Seen::BytesAppended(1),
            Seen::PageSubtitle(1, 1, b"WEBVTT".to_vec()),
        ]
    );
    let counters = translator.counters();
    assert_eq!(counters.page_subtitle_documents, 1);
    assert_eq!(counters.ignored_messages, 0);
}

#[test]
fn bytes_out_of_order_refuse_only_that_track() {
    let (mut translator, _log, _failure) = fake_translator();
    let notices = run(
        &mut translator,
        [
            offered(1, 1, 4),
            offered(1, 2, 2),
            subtitle_bytes(1, 1, 1, b"abc"),
            subtitle_bytes(1, 2, 0, b"ok"),
        ],
    );
    assert_eq!(
        seen(&notices),
        vec![Seen::PageSubtitle(1, 2, b"ok".to_vec())]
    );
    assert_eq!(translator.counters().page_subtitle_refusals, 1);
}

#[test]
fn an_unfinished_document_dies_with_its_session_instance() {
    let (mut translator, _log, _failure) = fake_translator();
    let notices = run(
        &mut translator,
        [
            offered(1, 1, 4),
            subtitle_bytes(1, 1, 0, b"ab"),
            SessionInput::Ended(session(1)),
            // Новый экземпляр начинает с нуля: продолжение прежнего документа ему чужое.
            subtitle_bytes(2, 1, 2, b"cd"),
        ],
    );
    assert_eq!(seen(&notices), vec![Seen::Sealed(1)]);
    assert_eq!(translator.counters().page_subtitle_refusals, 1);
}

#[test]
fn an_offer_over_the_session_budget_is_counted_as_a_refusal() {
    let (mut translator, _log, _failure) = fake_translator();
    let document_limit = browser_bridge_protocol::limits::MAX_PAGE_SUBTITLE_DOCUMENT_BYTES;
    let budget = browser_bridge_protocol::limits::MAX_PAGE_SUBTITLE_SESSION_BYTES;
    let fitting = u32::try_from(budget / document_limit).expect("a few documents");
    let mut inputs: Vec<SessionInput> = (1..=fitting)
        .map(|number| offered(1, number, document_limit))
        .collect();
    inputs.push(offered(1, fitting + 1, 1));
    run(&mut translator, inputs);
    assert_eq!(translator.counters().page_subtitle_refusals, 1);
}
