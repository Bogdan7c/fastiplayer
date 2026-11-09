//! Субтитры страницы у сторожа (протокол 1.5, решения 270–274): предложение дорожки,
//! байты документа рамками вида 3, замена, предел дорожек, версия и присутствие страницы.

use super::*;
use crate::limits::MAX_PAGE_SUBTITLE_TRACKS;
use crate::session::page_subtitles::{
    PageSubtitleByteLength, PageSubtitleDisplay, PageSubtitleLabel, PageSubtitleLanguage,
    PageSubtitleOrigin, PageSubtitleTrackId, SubtitleFormatHint,
};
use crate::time::ByteOffset;
use crate::version::ProtocolVersion;

fn track(number: u32) -> PageSubtitleTrackId {
    match PageSubtitleTrackId::new(number) {
        Ok(track) => track,
        Err(violation) => panic!("дорожка {number}: {violation}"),
    }
}

fn offered(number: u32, byte_length: u64) -> PageSessionMessage {
    let Ok(byte_length) = PageSubtitleByteLength::new(byte_length) else {
        panic!("длина {byte_length} должна проходить проверку");
    };
    PageSessionMessage::SubtitleTrackOffered {
        track: track(number),
        language: PageSubtitleLanguage::Unknown {},
        label: PageSubtitleLabel::None {},
        format_hint: SubtitleFormatHint::WebVtt,
        origin: PageSubtitleOrigin::PageDownload,
        page_display: PageSubtitleDisplay::Showing,
        byte_length,
    }
}

fn subtitle_bytes(number: u32, offset: u64) -> PageSubtitleBytesHeader {
    let Ok(offset) = ByteOffset::new(offset) else {
        panic!("смещение {offset} должно проходить проверку");
    };
    PageSubtitleBytesHeader {
        track: track(number),
        offset,
    }
}

/// Сторож с предложенной дорожкой 1 длиной 10 байт.
fn gate_with_offer() -> StreamGate {
    let mut gate = StreamGate::new();
    assert_eq!(
        gate.apply_message(&offered(1, 10)),
        Ok(GateDecision::Accept)
    );
    gate
}

#[test]
fn a_document_is_accepted_in_order_up_to_its_exact_length_and_then_it_is_complete() {
    let mut gate = gate_with_offer();
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 0), b"WEBVT"),
        Ok(GateDecision::Accept)
    );
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 5), b"T\n\n00"),
        Ok(GateDecision::Accept)
    );
    // Пришло ровно 10 байт: документ готов, ещё одна рамка — нарушение.
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 10), b"x"),
        Err(StreamViolation::PageSubtitleDocumentComplete)
    );
}

#[test]
fn bytes_for_a_track_that_was_never_offered_are_a_violation() {
    let mut gate = gate_with_offer();
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(2, 0), b"WEBVTT"),
        Err(StreamViolation::PageSubtitleTrackNotOffered)
    );
    // Чужая рамка не тронула учёт дорожки 1: её документ идёт с 0, как и шёл.
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 0), b"WEBVTT"),
        Ok(GateDecision::Accept)
    );
}

#[test]
fn an_offset_gap_or_repeat_names_the_expected_and_received_offsets_and_changes_nothing() {
    let mut gate = gate_with_offer();
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 0), b"WEBV"),
        Ok(GateDecision::Accept)
    );
    // Пропуск куска.
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 6), b"T\n"),
        Err(StreamViolation::PageSubtitleOffsetGap {
            expected: 4,
            received: 6
        })
    );
    // Повтор куска.
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 0), b"WEBV"),
        Err(StreamViolation::PageSubtitleOffsetGap {
            expected: 4,
            received: 0
        })
    );
    // Отвергнутые рамки ничего не сдвинули.
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 4), b"TT"),
        Ok(GateDecision::Accept)
    );
}

#[test]
fn a_frame_running_past_the_declared_length_is_a_violation_and_the_document_stays_open() {
    let mut gate = gate_with_offer();
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 0), b"WEBVTT\n\n0"),
        Ok(GateDecision::Accept)
    );
    // Остаток — 1 байт, рамка несёт 2.
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 9), b"0:"),
        Err(StreamViolation::PageSubtitleBeyondByteLength)
    );
    // Последний байт ровно по остатку принимается.
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 9), b"0"),
        Ok(GateDecision::Accept)
    );
}

#[test]
fn a_repeated_offer_replaces_a_partial_or_complete_document_and_restarts_at_zero() {
    let mut gate = gate_with_offer();
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 0), b"WEBV"),
        Ok(GateDecision::Accept)
    );
    // Замена недокачанного: новое предложение другой длины, смещение снова с 0.
    assert_eq!(gate.apply_message(&offered(1, 3)), Ok(GateDecision::Accept));
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 4), b"x"),
        Err(StreamViolation::PageSubtitleOffsetGap {
            expected: 0,
            received: 4
        })
    );
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 0), b"abc"),
        Ok(GateDecision::Accept)
    );
    // Замена готового документа тоже разрешена в любой момент.
    assert_eq!(gate.apply_message(&offered(1, 2)), Ok(GateDecision::Accept));
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 0), b"de"),
        Ok(GateDecision::Accept)
    );
}

#[test]
fn at_most_64_distinct_tracks_per_session_and_replacing_one_takes_no_new_place() {
    let mut gate = StreamGate::new();
    let limit = u32::try_from(MAX_PAGE_SUBTITLE_TRACKS).unwrap_or(u32::MAX);
    for number in 1..=limit {
        assert_eq!(
            gate.apply_message(&offered(number, 1)),
            Ok(GateDecision::Accept),
            "дорожка {number}"
        );
    }
    assert_eq!(
        gate.apply_message(&offered(limit + 1, 1)),
        Err(StreamViolation::TooManyPageSubtitleTracks)
    );
    // Замена уже известной дорожки на пределе места не занимает.
    assert_eq!(gate.apply_message(&offered(1, 5)), Ok(GateDecision::Accept));
    // Отвергнутая 65-я дорожка не завелась: её байты — «не предлагали».
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(limit + 1, 0), b"x"),
        Err(StreamViolation::PageSubtitleTrackNotOffered)
    );
}

#[test]
fn below_1_5_both_the_offer_and_the_frame_are_not_part_of_the_agreed_version() {
    let mut gate = StreamGate::for_agreed_version(ProtocolVersion::V1_4);
    assert_eq!(
        gate.apply_message(&offered(1, 10)),
        Err(StreamViolation::MessageNotInAgreedVersion {
            agreed: ProtocolVersion::V1_4,
            introduced_in: ProtocolVersion::V1_5,
        })
    );
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 0), b"WEBVTT"),
        Err(StreamViolation::DataFrameNotInAgreedVersion {
            agreed: ProtocolVersion::V1_4,
            introduced_in: ProtocolVersion::V1_5,
        })
    );
}

#[test]
fn subtitle_documents_do_not_touch_generations_or_source_buffers() {
    let mut gate = gate_with_generation(1);
    let before = gate.clone();
    assert_eq!(gate.apply_message(&offered(1, 6)), Ok(GateDecision::Accept));
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 0), b"WEBVTT"),
        Ok(GateDecision::Accept)
    );
    assert_eq!(gate.current_generation(), before.current_generation());
    // Поток MSE продолжается с того же номера рамки, как без субтитров.
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(1, 0, 0)),
        Ok(GateDecision::Accept)
    );
    // И документ переживает смену поколения: дорожку можно продолжать и после неё.
    assert_eq!(gate.apply_message(&offered(2, 4)), Ok(GateDecision::Accept));
    assert_eq!(gate.apply_message(&started(2)), Ok(GateDecision::Accept));
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(2, 0), b"abcd"),
        Ok(GateDecision::Accept)
    );
}

#[test]
fn while_the_page_is_away_neither_offers_nor_subtitle_bytes_are_allowed() {
    let mut gate = gate_with_offer();
    let page_left = PageSessionMessage::PageLeft {
        reason: TabGoneReason::Closed,
    };
    assert_eq!(gate.apply_message(&page_left), Ok(GateDecision::Accept));
    assert_eq!(
        gate.apply_message(&offered(2, 4)),
        Err(StreamViolation::PageIsAway)
    );
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 0), b"WEBVTT"),
        Err(StreamViolation::PageIsAway)
    );
    // Вернулась — недокачанный документ можно продолжить с того же места.
    assert_eq!(
        gate.apply_message(&PageSessionMessage::PageReturned {}),
        Ok(GateDecision::Accept)
    );
    assert_eq!(
        gate.admit_page_subtitle_bytes(&subtitle_bytes(1, 0), b"WEBVTT"),
        Ok(GateDecision::Accept)
    );
}
