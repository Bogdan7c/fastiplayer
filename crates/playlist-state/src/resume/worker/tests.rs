use super::*;

#[test]
fn disconnected_wake_endpoint_stops_worker_without_spin() {
    let directory = tempfile::tempdir().expect("temp directory");
    let store = Arc::new(PlaylistResumeStore::new(
        directory.path().join("disconnected-worker-resume.json"),
    ));
    let shared = Arc::new(Mutex::new(SharedState::new()));
    let shutdown_requested = Arc::new(AtomicBool::new(false));
    let (wake_tx, wake_rx) = mpsc::sync_channel(1);
    let (completion_tx, _completion_rx) = mpsc::sync_channel(1);
    let (exited_tx, exited_rx) = mpsc::sync_channel(1);
    let join_handle = thread::Builder::new()
        .name("playlist-resume-disconnect-regression".to_owned())
        .spawn(move || {
            run_worker(store, shared, shutdown_requested, wake_rx, completion_tx);
            let _exit_is_observed = exited_tx.send(());
        })
        .expect("start resume disconnect regression worker");

    drop(wake_tx);

    exited_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("disconnected wake endpoint должен остановить worker без spin");
    join_handle
        .join()
        .expect("resume disconnect regression worker не должен panic");

    // Второй вызов выполняет тот же production loop в test-owner thread: это делает exact
    // disconnected exit наблюдаемым без зависимости от thread-local coverage flush timing.
    let direct_store = Arc::new(PlaylistResumeStore::new(
        directory
            .path()
            .join("direct-disconnected-worker-resume.json"),
    ));
    let direct_shared = Arc::new(Mutex::new(SharedState::new()));
    let direct_shutdown_requested = Arc::new(AtomicBool::new(false));
    let (direct_wake_tx, direct_wake_rx) = mpsc::sync_channel(1);
    let (direct_completion_tx, direct_completion_rx) = mpsc::sync_channel(1);
    // Один wake без pending snapshot детерминированно проверяет возврат production loop к ожиданию.
    direct_wake_tx
        .try_send(())
        .expect("stale wake должен поместиться в пустой bounded channel");
    // Disconnect после stale wake обязан завершить worker без fake shutdown completion.
    drop(direct_wake_tx);

    run_worker(
        direct_store,
        direct_shared,
        direct_shutdown_requested,
        direct_wake_rx,
        direct_completion_tx,
    );

    assert_eq!(
        direct_completion_rx.try_recv(),
        Err(mpsc::TryRecvError::Disconnected),
        "wake disconnect не должен маскироваться под shutdown completion",
    );
}
