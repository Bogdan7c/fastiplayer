//! Записанные потоки протокола (`fixtures/streams`) через настоящий сервер:
//! в приёмник должны дойти ровно те байты, что записаны в ожидании фикстуры.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use browser_bridge_protocol::SessionCloseReason;
use browser_bridge_session::{BrowserSessionServer, SessionLifecycleEvent};
use browser_bridge_session::{CountingByteSink, SessionByteSink, SessionEndReason};

use crate::support::{
    Env, RecordingEvents, SequenceTokens, Watched, attach, read_close_code, send_binary, send_text,
    test_policy,
};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../browser-bridge-protocol/fixtures")
}

/// Один шаг записанного потока.
enum Step {
    Text(String),
    Binary(Vec<u8>),
}

/// Что обещает фикстура.
struct Recorded {
    steps: Vec<Step>,
    /// (поколение, SourceBuffer) → ожидаемые байты.
    expected: BTreeMap<(u32, u16), Vec<u8>>,
    dropped_frames: u64,
    dropped_messages: u64,
}

fn load(stream: &str) -> Recorded {
    let dir = fixtures_dir().join("streams").join(stream);
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("stream.json")).unwrap()).unwrap();
    let steps = manifest["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| match (step.get("text"), step.get("binary")) {
            (Some(file), None) => {
                Step::Text(std::fs::read_to_string(dir.join(file.as_str().unwrap())).unwrap())
            }
            (None, Some(file)) => {
                Step::Binary(std::fs::read(dir.join(file.as_str().unwrap())).unwrap())
            }
            _ => panic!("шаг должен быть text или binary"),
        })
        .collect();
    let mut expected: BTreeMap<(u32, u16), Vec<u8>> = BTreeMap::new();
    for admitted in manifest["expect"]["admitted"].as_array().unwrap() {
        let media = std::fs::read(
            fixtures_dir()
                .join("media")
                .join(admitted["media"].as_str().unwrap()),
        )
        .unwrap();
        let buffer = expected
            .entry((
                u32::try_from(admitted["generation"].as_u64().unwrap()).unwrap(),
                u16::try_from(admitted["source_buffer"].as_u64().unwrap()).unwrap(),
            ))
            .or_default();
        for range in admitted["ranges"].as_array().unwrap() {
            let start = usize::try_from(range[0].as_u64().unwrap()).unwrap();
            let end = usize::try_from(range[1].as_u64().unwrap()).unwrap();
            buffer.extend_from_slice(&media[start..end]);
        }
    }
    Recorded {
        steps,
        expected,
        dropped_frames: manifest["expect"]["dropped_frames"].as_u64().unwrap(),
        dropped_messages: manifest["expect"]["dropped_messages"].as_u64().unwrap(),
    }
}

/// Проигрывает поток через сервер и сверяет итог с фикстурой.
fn replay_and_verify(stream: &str) {
    let recorded = load(stream);
    let env = Env::start();
    let opened = env.open("replay");
    let mut socket = attach(env.port(), "replay", &opened.token);

    let mut text_steps = 0_usize;
    for step in &recorded.steps {
        match step {
            Step::Text(json) => {
                text_steps += 1;
                send_text(&mut socket, json);
            }
            Step::Binary(frame) => send_binary(&mut socket, frame.clone()),
        }
    }
    // Конец проигрыша: `tab_gone` обрабатывается после всего, что отправлено раньше,
    // поэтому закрытие 4002 значит «сервер переварил весь поток».
    send_text(&mut socket, r#"{"type":"tab_gone","reason":"closed"}"#);
    assert_eq!(
        read_close_code(&mut socket),
        SessionCloseReason::TabGone.close_code()
    );

    let received = env.bytes.received.get();
    assert_eq!(
        received.source_buffers, recorded.expected,
        "байты SourceBuffer"
    );
    let seen = env.events.seen.get();
    let expected_messages = text_steps - usize::try_from(recorded.dropped_messages).unwrap() + 1; // + tab_gone
    assert_eq!(
        seen.page_messages.len(),
        expected_messages,
        "сообщения страницы"
    );

    let report = env.server.shutdown();
    assert_eq!(report.dropped_stale_frames, recorded.dropped_frames);
    assert_eq!(report.dropped_stale_messages, recorded.dropped_messages);
    assert_eq!(report.delivered_chunks, received.frames as u64);
    assert_eq!(report.dropped_page_messages, 0);
    assert_eq!(report.detached_threads, 0);
}

#[test]
fn two_source_buffers_with_mid_box_cuts_arrive_byte_for_byte() {
    replay_and_verify("two-source-buffers-mid-box-cuts");
}

#[test]
fn stale_generation_tail_is_dropped_and_only_current_bytes_arrive() {
    replay_and_verify("generation-switch-drops-stale");
}

#[test]
fn muxed_single_source_buffer_webm_arrives_byte_for_byte() {
    replay_and_verify("muxed-av-single-source-buffer");
}

#[test]
fn session_keeps_receiving_bytes_after_the_entry_side_is_gone() {
    // Вход (стойка, D-Bus) отработал и исчез: ручка управления уничтожена, но
    // сервер и сеанс живут (инвариант 3).
    let recorded = load("two-source-buffers-mid-box-cuts");
    let env = Env::start();
    let port = env.port();
    let token = {
        let control = env.control.clone();
        let opened = control
            .open_session(&crate::support::open_request("alive"), Instant::now())
            .unwrap();
        drop(control);
        opened.token
    };
    let Env {
        server,
        control,
        bytes,
        events,
    } = env;
    drop(control);
    let mut socket = attach(port, "alive", &token);
    for step in &recorded.steps {
        match step {
            Step::Text(json) => send_text(&mut socket, json),
            Step::Binary(frame) => send_binary(&mut socket, frame.clone()),
        }
    }
    let total: usize = recorded.expected.values().map(Vec::len).sum();
    let received = bytes.received.wait_for(
        "все байты дошли без стойки входа",
        |received| {
            received
                .source_buffers
                .values()
                .map(Vec::len)
                .sum::<usize>()
                >= total
        },
    );
    assert_eq!(received.source_buffers, recorded.expected);
    drop(events);
    server.shutdown();
}

#[test]
fn counting_sink_stub_counts_the_same_replay_per_source_buffer() {
    // Заглушка 02 на настоящем сервере: те же байты, но только счётчики.
    let recorded = load("two-source-buffers-mid-box-cuts");
    let counting = std::sync::Arc::new(CountingByteSink::new());
    let events = std::sync::Arc::new(RecordingEvents {
        seen: Watched::new(),
        always_full: false,
    });
    let server = BrowserSessionServer::start(
        test_policy(),
        Box::new(SequenceTokens(0)),
        std::sync::Arc::clone(&counting) as std::sync::Arc<dyn SessionByteSink>,
        events.clone(),
    )
    .unwrap();
    let opened = server
        .control()
        .open_session(&crate::support::open_request("count"), Instant::now())
        .unwrap();
    let mut socket = attach(server.port().get(), "count", &opened.token);
    for step in &recorded.steps {
        match step {
            Step::Text(json) => send_text(&mut socket, json),
            Step::Binary(frame) => send_binary(&mut socket, frame.clone()),
        }
    }
    send_text(&mut socket, r#"{"type":"tab_gone","reason":"closed"}"#);
    read_close_code(&mut socket);

    let snapshot = counting.snapshot();
    for ((generation, source_buffer), bytes) in &recorded.expected {
        let counters = snapshot
            .source_buffers
            .iter()
            .find(|((g, b), _)| g.get() == *generation && b.get() == *source_buffer)
            .map(|(_, counters)| *counters)
            .unwrap();
        assert_eq!(counters.bytes, bytes.len() as u64);
    }
    // Сеанс закончился (tab_gone) — приёмнику сообщили освободить буферы.
    assert_eq!(
        snapshot.ended_sessions,
        vec![crate::support::session("count", 1)]
    );
    let ended = events.seen.get().lifecycle;
    assert!(ended.contains(&SessionLifecycleEvent::Ended {
        session: crate::support::session("count", 1),
        reason: SessionEndReason::ClosedByPlayer(SessionCloseReason::TabGone),
    }));
    server.shutdown();
}
