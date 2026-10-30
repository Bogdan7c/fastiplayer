use std::sync::mpsc;
use std::time::Instant;

use browser_bridge_protocol::session::page::{PagePlaybackRequest, SeekCause};
use browser_bridge_protocol::{
    MediaTime, PageSessionMessage, SeekId, SessionKey, StreamGeneration,
};
use browser_bridge_session::{SessionEventSink, SessionInstance, SessionRef};
use pushed_media_source::{PushedSourcePolicy, PushedStreamStartError};

use super::*;
use crate::browser_session::inbox::{InboxCapacity, ParkingWake};
use crate::browser_session::intake_notice::{IntakeNotice, PageIntent};
use crate::browser_session::stream_queue::QueueLimits;
use crate::browser_session::translator::file_port::unused_file_factory;
use crate::browser_session::translator::stream_port::{PushedStreamFactory, StartedStream};

const JOIN_TIMEOUT: Duration = Duration::from_secs(10);

fn session() -> SessionRef {
    SessionRef {
        key: SessionKey::new("tab").expect("valid key"),
        instance: SessionInstance(1),
    }
}

struct Rig {
    wake: Arc<ParkingWake>,
    inbox: Arc<BrowserSessionInbox>,
    queue: Arc<OrderedSessionQueue>,
}

fn rig() -> Rig {
    let wake = Arc::new(ParkingWake::default());
    let inbox = Arc::new(BrowserSessionInbox::new(
        InboxCapacity::default(),
        wake.clone(),
    ));
    let queue = Arc::new(OrderedSessionQueue::new(
        QueueLimits::default(),
        Arc::clone(&inbox),
    ));
    Rig { wake, inbox, queue }
}

fn real_translator() -> ProtocolTranslator<PushedStreamFactory> {
    ProtocolTranslator::new(
        PushedStreamFactory::new(PushedSourcePolicy::recommended().expect("policy")),
        unused_file_factory(),
    )
}

/// Ждёт, как главный цикл: спит на пробуждении и забирает ящик, пока не наберёт `count`.
fn wait_for_notices(rig: &Rig, count: usize) -> Vec<IntakeNotice> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut collected = Vec::new();
    while collected.len() < count {
        assert!(Instant::now() < deadline, "notices did not arrive in time");
        rig.wake
            .wait(Some(Instant::now() + Duration::from_millis(100)));
        collected.extend(rig.inbox.take_batch().intake_notices);
    }
    collected
}

#[test]
fn page_input_flows_from_the_queue_through_the_thread_to_the_inbox_in_order() {
    let rig = rig();
    let intake = StreamIntake::start(
        Arc::clone(&rig.queue),
        Arc::clone(&rig.inbox),
        real_translator(),
    )
    .expect("intake thread");
    let target = MediaTime::from_micros(5_000_000).expect("time");
    for message in [
        PageSessionMessage::GenerationStarted {
            generation: StreamGeneration::FIRST,
        },
        PageSessionMessage::PagePlayback {
            request: PagePlaybackRequest::Pause {},
        },
        PageSessionMessage::SeekStarted {
            target_us: target,
            cause: SeekCause::PlayerSeek { seek_id: SeekId(3) },
        },
    ] {
        rig.queue
            .publish_page_message(&session(), message)
            .expect("the queue never refuses");
    }

    let notices = wait_for_notices(&rig, 3);
    assert!(
        matches!(&notices[0], IntakeNotice::NewGeneration(handoff)
            if handoff.session == session() && handoff.generation == StreamGeneration::FIRST),
        "{notices:?}"
    );
    assert!(matches!(
        &notices[1],
        IntakeNotice::PageIntent {
            intent: PageIntent::Pause,
            ..
        }
    ));
    assert!(matches!(
        &notices[2],
        IntakeNotice::PlayerSeekAcknowledged { seek_id: SeekId(3), target: seen, .. }
            if *seen == target
    ));
    assert_eq!(notices.len(), 3);
    assert!(notices.iter().all(|notice| *notice.session() == session()));

    assert!(matches!(
        intake.shut_down(JOIN_TIMEOUT),
        IntakeShutdown::Joined(_)
    ));
}

#[test]
fn an_idle_intake_thread_stops_and_joins_on_shutdown() {
    let rig = rig();
    let intake = StreamIntake::start(
        Arc::clone(&rig.queue),
        Arc::clone(&rig.inbox),
        real_translator(),
    )
    .expect("intake thread");
    let started = Instant::now();
    assert_eq!(
        intake.shut_down(JOIN_TIMEOUT),
        IntakeShutdown::Joined(Default::default())
    );
    assert!(started.elapsed() < JOIN_TIMEOUT);
}

/// Фабрика, которая зависает внутри создания потока, пока её не отпустят.
struct StuckFactory {
    release: mpsc::Receiver<()>,
}

impl StreamFactory for StuckFactory {
    fn start_stream(&mut self) -> Result<StartedStream, PushedStreamStartError> {
        let _ = self.release.recv();
        PushedStreamFactory::new(PushedSourcePolicy::recommended().expect("policy")).start_stream()
    }
}

#[test]
fn a_stuck_intake_thread_is_detached_instead_of_hanging_the_shutdown() {
    let rig = rig();
    let (release, released) = mpsc::channel();
    let intake = StreamIntake::start(
        Arc::clone(&rig.queue),
        Arc::clone(&rig.inbox),
        ProtocolTranslator::new(StuckFactory { release: released }, unused_file_factory()),
    )
    .expect("intake thread");
    rig.queue
        .publish_page_message(
            &session(),
            PageSessionMessage::GenerationStarted {
                generation: StreamGeneration::FIRST,
            },
        )
        .expect("accepted");
    // Дождаться, пока поток заберёт пачку и зависнет в фабрике.
    let deadline = Instant::now() + Duration::from_secs(20);
    while !rig.queue.is_empty_for_tests() {
        assert!(
            Instant::now() < deadline,
            "intake thread did not take the batch"
        );
        std::thread::yield_now();
    }

    let started = Instant::now();
    assert_eq!(
        intake.shut_down(Duration::from_millis(100)),
        IntakeShutdown::Detached
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    // Отпускаем поток: он выходит сам, увидев остановку.
    release.send(()).expect("release");
}

/// Фабрика, которая падает с паникой на первом потоке (дефект источника), а дальше
/// работает как настоящая.
struct PanickingOnceFactory {
    panicked: bool,
}

impl StreamFactory for PanickingOnceFactory {
    fn start_stream(&mut self) -> Result<StartedStream, PushedStreamStartError> {
        if !self.panicked {
            self.panicked = true;
            panic!("stream factory defect");
        }
        PushedStreamFactory::new(PushedSourcePolicy::recommended().expect("policy")).start_stream()
    }
}

fn other_session() -> SessionRef {
    SessionRef {
        key: SessionKey::new("tab-2").expect("valid key"),
        instance: SessionInstance(2),
    }
}

/// Паника разбора не убивает поток молча: экземпляр, чей вход разбирался, сразу
/// получает просьбу закрыться, его дальнейший вход выбрасывается, а следующий
/// сеанс разбирается как обычно.
#[test]
fn a_panic_while_translating_closes_that_session_and_the_thread_keeps_working() {
    let rig = rig();
    let intake = StreamIntake::start(
        Arc::clone(&rig.queue),
        Arc::clone(&rig.inbox),
        ProtocolTranslator::new(
            PanickingOnceFactory { panicked: false },
            unused_file_factory(),
        ),
    )
    .expect("intake thread");
    let started = || PageSessionMessage::GenerationStarted {
        generation: StreamGeneration::FIRST,
    };
    rig.queue
        .publish_page_message(&session(), started())
        .expect("accepted");
    let notices = wait_for_notices(&rig, 1);
    assert!(
        matches!(&notices[..], [IntakeNotice::CloseSession { session: closed, .. }]
            if *closed == session()),
        "{notices:?}"
    );

    // Вход закрываемого экземпляра больше не разбирается, другой сеанс — да.
    rig.queue
        .publish_page_message(
            &session(),
            PageSessionMessage::PagePlayback {
                request: PagePlaybackRequest::Pause {},
            },
        )
        .expect("accepted");
    rig.queue
        .publish_page_message(&other_session(), started())
        .expect("accepted");
    let notices = wait_for_notices(&rig, 1);
    assert!(
        matches!(&notices[..], [IntakeNotice::NewGeneration(handoff)]
            if handoff.session == other_session()),
        "{notices:?}"
    );

    match intake.shut_down(JOIN_TIMEOUT) {
        IntakeShutdown::Joined(counters) => {
            assert_eq!(counters.internal_failures, 1);
            assert_eq!(counters.closing_instance_inputs, 1);
        }
        other => panic!("the intake thread did not survive: {other:?}"),
    }
}
