//! Consumer не возвращает устаревший snapshot из промежутка latest replacement.

use super::*;

#[test]
fn latest_snapshot_waits_for_atomic_replacement_instead_of_returning_old_instance() {
    let (snapshot_tx, snapshot_rx) = bounded(SNAPSHOT_CHANNEL_CAPACITY);
    let publisher = LatestSnapshotPublisher::new(snapshot_tx, snapshot_rx.clone());
    let lock = Arc::clone(&publisher.publication_lock);
    let old_instance = MediaInstanceId::new_unique();
    let new_instance = MediaInstanceId::new_unique();
    let (ready_tx, ready_rx) = bounded(0);
    let (start_tx, start_rx) = bounded(0);
    let (result_tx, result_rx) = bounded(1);
    let reader = thread::spawn(move || {
        let (mut worker, ..) =
            worker_with_latest_handoff_for_tests(Arc::new(LatestPresentFrameHandoff::new()));
        worker.snapshot_rx = snapshot_rx;
        worker.snapshot_publication_lock = lock;
        worker.cached_snapshot.media_instance_id = Some(old_instance);
        ready_tx.send(()).unwrap();
        start_rx.recv().unwrap();
        result_tx
            .send(worker.latest_snapshot(FrameCounters::default()))
            .unwrap();
    });
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();

    // Удерживаем ровно producer-owned интервал drain -> replacement. Public
    // consumer должен ждать завершения замены даже при пока пустом канале.
    let replacement = publisher.publication_lock.lock().unwrap();
    start_tx.send(()).unwrap();
    let premature = result_rx.recv_timeout(Duration::from_millis(50));
    let mut installed = PlayerSnapshot::empty();
    installed.media_instance_id = Some(new_instance);
    publisher.snapshot_tx.send(installed).unwrap();
    drop(replacement);
    reader
        .join()
        .expect("snapshot reader finishes after publication");
    assert!(
        premature.is_err(),
        "reader must not return cached old instance during replacement"
    );
    assert_eq!(
        result_rx.recv().unwrap().media_instance_id,
        Some(new_instance)
    );
}
