//! Сборщик документов страницы: куски подряд дают документ, ошибки страницы отказывают ровно
//! одной дорожке.

use browser_bridge_protocol::session::PageSubtitleTrackId;

use super::*;

fn offer(track: u32, byte_length: u64, display: PageSubtitleDisplay) -> PageSubtitleOffer {
    PageSubtitleOffer {
        track: PageSubtitleTrackNumber::new(track),
        language: PageSubtitleLanguage::Tagged("en".to_owned()),
        label: PageSubtitleLabel::Text("English".to_owned()),
        format_hint: PageSubtitleFormatHint::WebVtt,
        display,
        source: PageSubtitleSource::PageDownload,
        byte_length,
    }
}

fn chunk(track: u32, offset: u64, payload: &[u8]) -> OwnedPageSubtitleChunk {
    OwnedPageSubtitleChunk {
        track: PageSubtitleTrackId::new(track).expect("valid track"),
        offset,
        payload: payload.to_vec(),
    }
}

fn completed(outcome: ChunkOutcome) -> PageSubtitleDocument {
    match outcome {
        ChunkOutcome::Completed(document) => document,
        other => panic!("expected a finished document, got {other:?}"),
    }
}

#[test]
fn chunks_in_order_become_the_whole_document_with_the_offer_metadata() {
    let mut assembly = PageSubtitleAssembly::default();
    assert_eq!(
        assembly.offer(offer(1, 10, PageSubtitleDisplay::ShowingOnPage)),
        OfferOutcome::Opened
    );
    assert_eq!(
        assembly.accept_chunk(chunk(1, 0, b"WEBVT")),
        ChunkOutcome::Incomplete
    );
    let document = completed(assembly.accept_chunk(chunk(1, 5, b"T\n\nab")));

    assert_eq!(&*document.bytes, b"WEBVTT\n\nab");
    assert_eq!(document.track, PageSubtitleTrackNumber::new(1));
    assert_eq!(document.display, PageSubtitleDisplay::ShowingOnPage);
    assert_eq!(document.format_hint, PageSubtitleFormatHint::WebVtt);
    assert_eq!(document.label.text(), Some("English"));
    assert_eq!(document.language.tag(), Some("en"));
    // Готовый документ сборщик не держит: следующий кусок того же номера — чужой.
    assert_eq!(assembly.open_track_count(), 0);
    assert_eq!(
        assembly.accept_chunk(chunk(1, 10, b"x")),
        ChunkOutcome::Refused(ChunkRefusal::NotOffered)
    );
}

#[test]
fn a_repeated_offer_replaces_the_unfinished_document() {
    let mut assembly = PageSubtitleAssembly::default();
    assembly.offer(offer(2, 8, PageSubtitleDisplay::NotShowingOnPage));
    assembly.accept_chunk(chunk(2, 0, b"OLD-"));
    assert_eq!(
        assembly.offer(offer(2, 3, PageSubtitleDisplay::ShowingOnPage)),
        OfferOutcome::ReplacedUnfinished
    );
    // Новый документ начинается с нуля; старые 4 байта забыты.
    let document = completed(assembly.accept_chunk(chunk(2, 0, b"new")));
    assert_eq!(&*document.bytes, b"new");
    assert_eq!(document.display, PageSubtitleDisplay::ShowingOnPage);
}

#[test]
fn a_gap_refuses_only_that_track_and_the_neighbour_still_finishes() {
    let mut assembly = PageSubtitleAssembly::default();
    assembly.offer(offer(1, 6, PageSubtitleDisplay::NotShowingOnPage));
    assembly.offer(offer(2, 4, PageSubtitleDisplay::NotShowingOnPage));
    assembly.accept_chunk(chunk(1, 0, b"abc"));
    assembly.accept_chunk(chunk(2, 0, b"12"));

    assert_eq!(
        assembly.accept_chunk(chunk(1, 4, b"ef")),
        ChunkOutcome::Refused(ChunkRefusal::OffsetMismatch {
            expected: 3,
            received: 4
        })
    );
    // Дорожка 1 выброшена целиком: её продолжение уже не принимается.
    assert_eq!(
        assembly.accept_chunk(chunk(1, 3, b"def")),
        ChunkOutcome::Refused(ChunkRefusal::NotOffered)
    );
    // Соседка не тронута.
    let neighbour = completed(assembly.accept_chunk(chunk(2, 2, b"34")));
    assert_eq!(&*neighbour.bytes, b"1234");
}

#[test]
fn bytes_past_the_declared_length_refuse_the_track() {
    let mut assembly = PageSubtitleAssembly::default();
    assembly.offer(offer(5, 4, PageSubtitleDisplay::NotShowingOnPage));
    assert_eq!(
        assembly.accept_chunk(chunk(5, 0, b"12345")),
        ChunkOutcome::Refused(ChunkRefusal::PastDeclaredLength)
    );
    assert_eq!(assembly.open_track_count(), 0);
}

#[test]
fn bytes_of_a_track_nobody_offered_are_refused_without_touching_open_tracks() {
    let mut assembly = PageSubtitleAssembly::default();
    assembly.offer(offer(1, 2, PageSubtitleDisplay::NotShowingOnPage));
    assert_eq!(
        assembly.accept_chunk(chunk(9, 0, b"zz")),
        ChunkOutcome::Refused(ChunkRefusal::NotOffered)
    );
    assert_eq!(assembly.open_track_count(), 1);
    assert_eq!(
        assembly.accept_chunk(chunk(1, 0, b"")),
        ChunkOutcome::Refused(ChunkRefusal::Empty)
    );
}

#[test]
fn more_open_tracks_than_the_protocol_allows_are_refused_but_a_repeat_still_replaces() {
    let mut assembly = PageSubtitleAssembly::default();
    let limit = u32::try_from(MAX_PAGE_SUBTITLE_TRACKS).expect("small limit");
    for track in 1..=limit {
        assert_eq!(
            assembly.offer(offer(track, 1, PageSubtitleDisplay::NotShowingOnPage)),
            OfferOutcome::Opened
        );
    }
    assert_eq!(
        assembly.offer(offer(limit + 1, 1, PageSubtitleDisplay::NotShowingOnPage)),
        OfferOutcome::RefusedTooManyTracks
    );
    assert_eq!(
        assembly.offer(offer(1, 1, PageSubtitleDisplay::NotShowingOnPage)),
        OfferOutcome::ReplacedUnfinished
    );
    assert_eq!(assembly.open_track_count(), MAX_PAGE_SUBTITLE_TRACKS);
}

#[test]
fn a_document_over_the_session_budget_is_refused_alone_and_the_others_still_work() {
    let mut assembly = PageSubtitleAssembly::with_session_budget(10);
    assembly.offer(offer(1, 6, PageSubtitleDisplay::NotShowingOnPage));
    completed(assembly.accept_chunk(chunk(1, 0, b"aaaaaa")));
    assert_eq!(
        assembly.offer(offer(2, 4, PageSubtitleDisplay::NotShowingOnPage)),
        OfferOutcome::Opened,
        "6 + 4 fits exactly"
    );
    assert_eq!(
        assembly.offer(offer(3, 1, PageSubtitleDisplay::NotShowingOnPage)),
        OfferOutcome::RefusedOverSessionBudget
    );
    // Отказанное не буферизуется: его байты чужие.
    assert_eq!(
        assembly.accept_chunk(chunk(3, 0, b"x")),
        ChunkOutcome::Refused(ChunkRefusal::NotOffered)
    );
    // Принятые дорожки живут: вторая докачивается.
    assert_eq!(
        &*completed(assembly.accept_chunk(chunk(2, 0, b"bbbb"))).bytes,
        b"bbbb"
    );
}

#[test]
fn replacing_a_big_document_by_a_small_one_frees_the_budget() {
    let mut assembly = PageSubtitleAssembly::with_session_budget(10);
    assembly.offer(offer(1, 8, PageSubtitleDisplay::NotShowingOnPage));
    completed(assembly.accept_chunk(chunk(1, 0, b"88888888")));
    assert_eq!(
        assembly.offer(offer(2, 4, PageSubtitleDisplay::NotShowingOnPage)),
        OfferOutcome::RefusedOverSessionBudget
    );
    // Замена считает новый размер вместо прежнего.
    assert_eq!(
        assembly.offer(offer(1, 2, PageSubtitleDisplay::NotShowingOnPage)),
        OfferOutcome::Opened
    );
    completed(assembly.accept_chunk(chunk(1, 0, b"22")));
    assert_eq!(
        assembly.offer(offer(2, 4, PageSubtitleDisplay::NotShowingOnPage)),
        OfferOutcome::Opened,
        "2 + 4 fits now"
    );
}

#[test]
fn unfinished_documents_count_against_the_budget_until_they_are_dropped() {
    let mut assembly = PageSubtitleAssembly::with_session_budget(10);
    assembly.offer(offer(1, 8, PageSubtitleDisplay::NotShowingOnPage));
    assert_eq!(
        assembly.offer(offer(2, 4, PageSubtitleDisplay::NotShowingOnPage)),
        OfferOutcome::RefusedOverSessionBudget,
        "8 declared but not yet received still counts"
    );
    // Дорожка 1 нарушила порядок и выброшена: её объявленная длина больше не держится.
    assembly.accept_chunk(chunk(1, 5, b"x"));
    assert_eq!(
        assembly.offer(offer(2, 4, PageSubtitleDisplay::NotShowingOnPage)),
        OfferOutcome::Opened
    );
}

#[test]
fn the_origin_of_the_offer_decides_whether_the_page_downloaded_the_document() {
    use browser_bridge_protocol::session::{
        PageSubtitleByteLength, PageSubtitleDisplay as WireDisplay, PageSubtitleLabel as WireLabel,
        PageSubtitleLanguage as WireLanguage, PageSubtitleOrigin, SubtitleFormatHint as WireHint,
    };

    let source_of = |origin| {
        let offer = PageSubtitleOffer::from_message(PageSessionMessage::SubtitleTrackOffered {
            track: PageSubtitleTrackId::new(1).expect("valid track"),
            language: WireLanguage::Unknown {},
            label: WireLabel::None {},
            format_hint: WireHint::WebVtt,
            origin,
            page_display: WireDisplay::Showing,
            byte_length: PageSubtitleByteLength::new(1).expect("valid length"),
        })
        .expect("an offer");
        let mut assembly = PageSubtitleAssembly::default();
        assembly.offer(offer);
        completed(assembly.accept_chunk(chunk(1, 0, b"x"))).source
    };

    assert_eq!(
        source_of(PageSubtitleOrigin::PageDownload),
        PageSubtitleSource::PageDownload
    );
    assert_eq!(
        source_of(PageSubtitleOrigin::TrackElement),
        PageSubtitleSource::PageElement
    );
    assert_eq!(
        source_of(PageSubtitleOrigin::TextTrackCues),
        PageSubtitleSource::PageElement
    );
}
