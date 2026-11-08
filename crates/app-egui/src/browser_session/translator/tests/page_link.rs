//! «Сирота» в переходнике (протокол 1.4, сессия 11b1): уход страницы запечатывает поток,
//! но экземпляр живёт — вернувшаяся страница начинает новое поколение того же сеанса.

use browser_bridge_protocol::session::PageReopenFailure;

use super::*;

fn page_left(instance: u64) -> SessionInput {
    message(
        instance,
        PageSessionMessage::PageLeft {
            reason: TabGoneReason::Closed,
        },
    )
}

#[test]
fn page_left_seals_the_stream_and_keeps_the_instance_for_a_new_generation() {
    let (mut translator, log, _failure) = fake_translator();
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            added(1, 1, 0, "video/mp4"),
            bytes(1, 1, 0, b"before"),
            page_left(1),
            message(1, PageSessionMessage::PageReturned {}),
            started(1, 2),
            added(1, 2, 0, "video/mp4"),
            bytes(1, 2, 0, b"after"),
        ],
    );

    // Поток поколения 1 оборван ровно один раз (уход), поток 2 — живой и получает байты.
    assert_eq!(
        log.calls(),
        vec![
            (1, WriterCall::AddLane(0, PushedContainer::FragmentedMp4)),
            (1, WriterCall::Append(0, b"before".to_vec())),
            (1, WriterCall::EndDownloadCutShort),
            (2, WriterCall::AddLane(0, PushedContainer::FragmentedMp4)),
            (2, WriterCall::Append(0, b"after".to_vec())),
        ]
    );
    // Уход — после данных, возврат — раньше нового поколения; конец экземпляра не объявлен.
    assert_eq!(
        seen(&notices),
        vec![
            Seen::NewGeneration(1, 1),
            Seen::BytesAppended(1),
            Seen::PageLink(1, PageLinkChange::Left(TabGoneReason::Closed)),
            Seen::PageLink(1, PageLinkChange::Returned),
            Seen::NewGeneration(1, 2),
            Seen::BytesAppended(1),
        ]
    );
}

#[test]
fn reopen_failure_is_passed_on_and_the_later_end_still_seals_nothing_twice() {
    let (mut translator, log, _failure) = fake_translator();
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            added(1, 1, 0, "video/mp4"),
            page_left(1),
            message(
                1,
                PageSessionMessage::PageReopenFailed {
                    reason: PageReopenFailure::NotTheSameVideo,
                },
            ),
            SessionInput::Ended(session(1)),
        ],
    );

    let cuts = log
        .calls()
        .into_iter()
        .filter(|(_, call)| *call == WriterCall::EndDownloadCutShort)
        .count();
    assert_eq!(
        cuts, 1,
        "поток запечатан уходом, конец экземпляра его не трогает"
    );
    assert_eq!(
        seen(&notices),
        vec![
            Seen::NewGeneration(1, 1),
            Seen::PageLink(1, PageLinkChange::Left(TabGoneReason::Closed)),
            Seen::PageLink(
                1,
                PageLinkChange::ReopenFailed(PageReopenFailure::NotTheSameVideo)
            ),
            Seen::Sealed(1),
        ]
    );
}

#[test]
fn page_link_of_an_older_instance_is_dropped() {
    let (mut translator, _log, _failure) = fake_translator();
    let notices = run(&mut translator, [started(2, 1), page_left(1)]);
    assert_eq!(seen(&notices), vec![Seen::NewGeneration(2, 1)]);
}
