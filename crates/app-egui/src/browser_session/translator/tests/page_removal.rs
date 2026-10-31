//! `source_buffer_removed` (MSE `remove()` страницы) → писатель нужной полосы, по
//! тем же правилам маршрута, что и другие сообщения SourceBuffer.

use super::*;

fn removed(instance: u64, number: u32, index: u16, start_us: u64, end: RemovalEnd) -> SessionInput {
    message(
        instance,
        PageSessionMessage::SourceBufferRemoved {
            generation: generation(number),
            source_buffer: buffer(index),
            start_us: MediaTime::from_micros(start_us).expect("time"),
            end,
        },
    )
}

fn until_us(time_us: u64) -> RemovalEnd {
    RemovalEnd::At {
        time_us: MediaTime::from_micros(time_us).expect("time"),
    }
}

#[test]
fn removal_reaches_the_writer_of_its_lane_with_the_exact_range_in_wire_order() {
    let (mut translator, log, _failure) = fake_translator();
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            added(1, 1, 0, "video/mp4"),
            added(1, 1, 1, "audio/mp4"),
            bytes(1, 1, 1, b"before"),
            removed(1, 1, 1, 5_000_000, until_us(7_500_000)),
            bytes(1, 1, 1, b"after"),
        ],
    );

    assert_eq!(
        log.calls(),
        vec![
            (1, WriterCall::AddLane(0, PushedContainer::FragmentedMp4)),
            (1, WriterCall::AddLane(1, PushedContainer::FragmentedMp4)),
            (1, WriterCall::Append(1, b"before".to_vec())),
            (
                1,
                WriterCall::NotePageRemoved(
                    1,
                    PageRemovedRange::new(
                        Duration::from_secs(5),
                        PageRemovedRangeEnd::At(Duration::from_millis(7_500)),
                    )
                )
            ),
            (1, WriterCall::Append(1, b"after".to_vec())),
        ]
    );
    // Стирание главному циклу ничего не сообщает: уведомлений не добавляет, и
    // подсказка «легли байты» вокруг него остаётся одной на серию.
    assert_eq!(
        seen(&notices),
        vec![Seen::NewGeneration(1, 1), Seen::BytesAppended(1)]
    );
    assert_eq!(translator.counters().ignored_messages, 0);
}

#[test]
fn removal_of_an_unknown_source_buffer_closes_the_session_once() {
    let (mut translator, log, _failure) = fake_translator();
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            added(1, 1, 0, "video/mp4"),
            removed(1, 1, 3, 0, RemovalEnd::EndOfTimeline {}),
            removed(1, 1, 0, 0, RemovalEnd::EndOfTimeline {}),
        ],
    );

    // Как байты неизвестного SourceBuffer: нарушение порядка → 4003, дальше вход
    // экземпляра в источник не идёт.
    assert_eq!(
        seen(&notices),
        vec![
            Seen::NewGeneration(1, 1),
            Seen::Close(1, SessionCloseReason::ProtocolViolation),
        ]
    );
    assert_eq!(
        log.calls(),
        vec![(1, WriterCall::AddLane(0, PushedContainer::FragmentedMp4))]
    );
    assert_eq!(translator.counters().closing_instance_inputs, 1);
}

#[test]
fn removal_of_an_old_generation_does_not_touch_the_new_one() {
    let (mut translator, log, _failure) = fake_translator();
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            added(1, 1, 0, "video/mp4"),
            started(1, 2),
            added(1, 2, 0, "video/webm"),
            removed(1, 1, 0, 0, RemovalEnd::EndOfTimeline {}),
        ],
    );

    assert_eq!(
        log.calls(),
        vec![
            (1, WriterCall::AddLane(0, PushedContainer::FragmentedMp4)),
            (2, WriterCall::AddLane(0, PushedContainer::WebM)),
        ]
    );
    assert_eq!(
        seen(&notices),
        vec![Seen::NewGeneration(1, 1), Seen::NewGeneration(1, 2)]
    );
    assert_eq!(translator.counters().stale_inputs, 1);
}

#[test]
fn removal_of_a_future_generation_is_a_protocol_violation() {
    let (mut translator, log, _failure) = fake_translator();
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            added(1, 1, 0, "video/mp4"),
            removed(1, 2, 0, 0, until_us(1_000_000)),
        ],
    );

    assert_eq!(
        seen(&notices),
        vec![
            Seen::NewGeneration(1, 1),
            Seen::Close(1, SessionCloseReason::ProtocolViolation),
        ]
    );
    assert_eq!(
        log.calls(),
        vec![(1, WriterCall::AddLane(0, PushedContainer::FragmentedMp4))]
    );
}
