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
}
