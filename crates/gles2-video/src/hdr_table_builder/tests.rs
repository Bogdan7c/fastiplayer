//! Порядок событий фонового сборщика на подставленной сборке с «воротами»: без гонок по времени.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests fail loudly on broken invariants"
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{BuildPoll, BuildRequestOutcome, HdrTableBuilder};
use crate::frame_color::RequiredHdrTable;
use crate::hdr::lut::{HdrLutKey, HdrLutTable};
use crate::hdr::{HdrReferenceLevels, HdrTransfer};

/// Подставная сборка: ждёт сигнала «можно» из канала, считает вызовы, кладёт в таблицу один байт
/// — номер белого, чтобы проверить, чья таблица пришла.
struct Gate {
    permits: Sender<()>,
    started: Arc<AtomicUsize>,
    build: Arc<super::BuildTable>,
}

fn gated_build() -> Gate {
    let (permits, permit_receiver): (Sender<()>, Receiver<()>) = channel();
    let permit_receiver = Mutex::new(permit_receiver);
    let started = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&started);
    let build: Arc<super::BuildTable> = Arc::new(move |_, levels: HdrReferenceLevels| {
        counter.fetch_add(1, Ordering::SeqCst);
        // Ждём разрешения; закрытый канал тоже отпускает (тест кончился).
        let _ = permit_receiver.lock().unwrap().recv();
        HdrLutTable {
            rgba: vec![levels.sdr_white_nits() as u8],
        }
    });
    Gate {
        permits,
        started,
        build,
    }
}

fn required(white: f32) -> RequiredHdrTable {
    let levels = HdrReferenceLevels::new(white, 1_000.0).unwrap();
    RequiredHdrTable {
        key: HdrLutKey::new(HdrTransfer::Pq, levels),
        transfer: HdrTransfer::Pq,
        levels,
    }
}

fn wait_for_result(builder: &mut HdrTableBuilder) -> BuildPoll {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match builder.poll() {
            BuildPoll::NotReady if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            other => return other,
        }
    }
}

fn wait_until(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "condition never became true");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn nothing_requested_means_nothing_pending() {
    let mut builder = HdrTableBuilder::new();
    assert!(!builder.has_pending_work());
    assert!(matches!(builder.poll(), BuildPoll::NotReady));
    assert_eq!(builder.superseded_requests(), 0);
}

#[test]
fn request_runs_in_background_and_poll_does_not_block_until_ready() {
    let gate = gated_build();
    let mut builder = HdrTableBuilder::with_build(gate.build);
    assert!(matches!(
        builder.request(required(100.0)),
        BuildRequestOutcome::Started
    ));
    // Сборка стоит на воротах: poll обязан вернуться сразу «не готово».
    assert!(matches!(builder.poll(), BuildPoll::NotReady));
    assert!(builder.has_pending_work());
    gate.permits.send(()).unwrap();
    let BuildPoll::Ready(built) = wait_for_result(&mut builder) else {
        panic!("expected the table");
    };
    assert_eq!(built.key, required(100.0).key);
    assert_eq!(built.table.rgba, vec![100]);
    assert!(!builder.has_pending_work());
}

/// Серия быстрых смен: строятся первая (она уже в полёте) и последняя; промежуточные — никогда.
#[test]
fn rapid_requests_build_only_first_and_last() {
    let gate = gated_build();
    let mut builder = HdrTableBuilder::with_build(gate.build);
    assert!(matches!(
        builder.request(required(101.0)),
        BuildRequestOutcome::Started
    ));
    for white in [102.0, 103.0, 104.0, 105.0] {
        assert!(matches!(
            builder.request(required(white)),
            BuildRequestOutcome::Queued
        ));
    }
    // Из четырёх запомненных три заменены.
    assert_eq!(builder.superseded_requests(), 3);
    gate.permits.send(()).unwrap();
    gate.permits.send(()).unwrap();
    // Первая готова, но устарела: выброшена; следом сама стартует последняя.
    assert!(matches!(
        wait_for_result(&mut builder),
        BuildPoll::Superseded
    ));
    let BuildPoll::Ready(built) = wait_for_result(&mut builder) else {
        panic!("expected the last table");
    };
    assert_eq!(built.key, required(105.0).key);
    assert_eq!(built.table.rgba, vec![105]);
    // Построено ровно две сборки, не пять.
    assert_eq!(gate.started.load(Ordering::SeqCst), 2);
    assert_eq!(builder.superseded_requests(), 4);
    assert!(!builder.has_pending_work());
}

/// Повтор уже запомненного ключа не «заменяет» его: счётчик заменённых запросов честен
/// (заменой считается только вытеснение ДРУГОГО ключа).
#[test]
fn repeating_the_queued_key_is_not_counted_as_a_superseded_request() {
    let gate = gated_build();
    let mut builder = HdrTableBuilder::with_build(gate.build);
    builder.request(required(101.0));
    assert!(matches!(
        builder.request(required(102.0)),
        BuildRequestOutcome::Queued
    ));
    assert!(matches!(
        builder.request(required(102.0)),
        BuildRequestOutcome::Queued
    ));
    assert_eq!(builder.superseded_requests(), 0, "nothing was displaced");

    assert!(matches!(
        builder.request(required(103.0)),
        BuildRequestOutcome::Queued
    ));
    assert_eq!(builder.superseded_requests(), 1, "102 was displaced by 103");

    gate.permits.send(()).unwrap();
    gate.permits.send(()).unwrap();
    assert!(matches!(
        wait_for_result(&mut builder),
        BuildPoll::Superseded
    ));
    let BuildPoll::Ready(built) = wait_for_result(&mut builder) else {
        panic!("expected the last table");
    };
    assert_eq!(built.key, required(103.0).key);
    assert_eq!(gate.started.load(Ordering::SeqCst), 2);
}

#[test]
fn repeating_the_in_flight_key_changes_nothing_and_drops_the_tail() {
    let gate = gated_build();
    let mut builder = HdrTableBuilder::with_build(gate.build);
    builder.request(required(101.0));
    builder.request(required(102.0));
    // Вернулись к тому, что уже строится: хвост не нужен, вторая сборка не стартует.
    assert!(matches!(
        builder.request(required(101.0)),
        BuildRequestOutcome::AlreadyInFlight
    ));
    gate.permits.send(()).unwrap();
    let BuildPoll::Ready(built) = wait_for_result(&mut builder) else {
        panic!("expected the first table");
    };
    assert_eq!(built.key, required(101.0).key);
    assert_eq!(gate.started.load(Ordering::SeqCst), 1);
    assert_eq!(builder.superseded_requests(), 1);
}

#[test]
fn withdrawn_request_discards_the_finished_table() {
    let gate = gated_build();
    let mut builder = HdrTableBuilder::with_build(gate.build);
    builder.request(required(101.0));
    builder.request(required(102.0));
    builder.withdraw();
    assert_eq!(builder.superseded_requests(), 1, "queued tail dropped");
    gate.permits.send(()).unwrap();
    assert!(matches!(
        wait_for_result(&mut builder),
        BuildPoll::Superseded
    ));
    assert_eq!(gate.started.load(Ordering::SeqCst), 1);
    assert!(!builder.has_pending_work());
}

/// Отозванный запрос не держит приложение в перерисовке, хотя сборка ещё идёт: результат
/// всё равно выбросят, картинка из-за него не изменится.
#[test]
fn withdrawn_request_is_not_pending_work_even_while_the_build_still_runs() {
    let gate = gated_build();
    let mut builder = HdrTableBuilder::with_build(gate.build);
    builder.request(required(101.0));
    assert!(builder.has_pending_work(), "wanted and in flight");

    builder.withdraw();

    assert!(
        !builder.has_pending_work(),
        "nobody waits for the discarded table"
    );
    gate.permits.send(()).unwrap();
    assert!(matches!(
        wait_for_result(&mut builder),
        BuildPoll::Superseded
    ));
}

#[test]
fn panicking_worker_is_reported_and_allows_a_new_request() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let build: Arc<super::BuildTable> = Arc::new(move |_, _| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("simulated worker crash");
        }
        HdrLutTable { rgba: vec![7] }
    });
    let mut builder = HdrTableBuilder::with_build(build);
    builder.request(required(100.0));
    assert!(matches!(
        wait_for_result(&mut builder),
        BuildPoll::WorkerLost
    ));
    assert!(!builder.has_pending_work());
    builder.request(required(100.0));
    let BuildPoll::Ready(built) = wait_for_result(&mut builder) else {
        panic!("retry must succeed");
    };
    assert_eq!(built.table.rgba, vec![7]);
}

/// Уничтожение сборщика во время сборки не ждёт её и не оставляет поток: после снятия ворот
/// поток доходит до конца и отпускает свою ссылку на функцию сборки.
#[test]
fn dropping_the_builder_mid_build_detaches_a_worker_that_still_finishes() {
    let gate = gated_build();
    let witness = Arc::clone(&gate.build);
    let mut builder = HdrTableBuilder::with_build(gate.build);
    builder.request(required(100.0));
    wait_until(|| gate.started.load(Ordering::SeqCst) == 1);
    let dropped_at = Instant::now();
    drop(builder);
    assert!(
        dropped_at.elapsed() < Duration::from_secs(1),
        "drop must not wait for the build"
    );
    // Поток ещё держит функцию (стоит на воротах): ссылок две.
    assert_eq!(Arc::strong_count(&witness), 2);
    gate.permits.send(()).unwrap();
    wait_until(|| Arc::strong_count(&witness) == 1);
}
