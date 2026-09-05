//! Незавершённый writer остаётся у owner-а до реального durable export.

use super::*;

#[test]
fn pending_writer_drain_keeps_owner_until_durable_file_is_published() {
    let directory = tempdir().expect("export directory");
    let target = directory.path().join("pending.m3u8");
    let prepared = continuation(19, target.clone());
    let (release, pending) = std::sync::mpsc::sync_channel(0);
    let job = PlaylistExportJob::spawn_runner(wake_port(), 19, "pending-export", move |_| {
        pending
            .recv_timeout(Duration::from_secs(2))
            .expect("owner releases writer after pending drain");
        write_prepared_export(prepared)
    })
    .expect("spawn real export writer");
    let mut owner = PlaylistExportIoOwner {
        wake_port: wake_port(),
        generation: 19,
        job: Some(job),
    };

    // До rendezvous ни завершение потока, ни terminal mailbox ещё невозможны.
    assert!(owner.drain().is_none());
    assert!(owner.is_open());
    assert!(!target.exists());
    release.send(()).expect("release live writer");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let completion = loop {
        if let Some(completion) = owner.drain() {
            break completion;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "export completion deadline"
        );
        thread::yield_now();
    };
    assert!(matches!(
        completion,
        PlaylistExportJobCompletion::Written {
            generation: 19,
            durability: PlaylistExportDurability::Durable,
            ..
        }
    ));
    assert_eq!(
        fs::read(target).expect("read durable playlist"),
        b"#EXTM3U\n"
    );
    assert!(!owner.is_open());
    assert!(owner.drain().is_none(), "terminal delivered exactly once");
}
