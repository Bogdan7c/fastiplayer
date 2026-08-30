use std::sync::{Arc, mpsc};

use super::super::*;
use crate::app_wake::{AppWakeEvent, AppWakeOwner, WakeEmitter};

struct ChannelWakeEmitter(mpsc::SyncSender<()>);

impl WakeEmitter for ChannelWakeEmitter {
    fn emit(&self, _event: AppWakeEvent) -> Result<(), ()> {
        self.0.send(()).map_err(|_| ())
    }
}

#[test]
fn supersede_cancels_in_flight_job_before_late_completion_can_stage() {
    let (wake_sender, wake_receiver) = mpsc::sync_channel(0);
    let wake_port = AppWakePort::new(
        AppWakeOwner::PlaylistRuntime,
        Arc::new(ChannelWakeEmitter(wake_sender)),
    );
    let (started_sender, started_receiver) = mpsc::sync_channel(0);
    let (release_sender, release_receiver) = mpsc::sync_channel(0);
    let job = PlaylistImportJob::spawn_runner(
        wake_port.clone(),
        "playlist-import-supersede-test",
        move |worker_cancel, expansion_cancellation| {
            started_sender.send(()).expect("publish worker start");
            release_receiver.recv().expect("release worker");
            if worker_cancel.load(Ordering::Acquire) || expansion_cancellation.is_cancelled() {
                PlaylistImportJobCompletion::Cancelled
            } else {
                panic!("superseded worker must observe cancellation")
            }
        },
    )
    .expect("spawn test import job");
    let mut owner = PlaylistImportIoOwner {
        wake_port,
        job: Some(job),
    };
    started_receiver.recv().expect("worker started");

    owner.cancel_active();
    // Удерживаем worker до первого drain: owner не должен синтезировать terminal сам.
    assert!(owner.drain().is_none());
    release_sender.send(()).expect("release worker");
    // Mailbox записывает completion до wake, поэтому rendezvous заменяет polling без таймингов.
    wake_receiver.recv().expect("completion wake");
    let completion = owner.drain().expect("cancelled terminal completion");

    assert!(matches!(completion, PlaylistImportJobCompletion::Cancelled));
    assert!(!owner.is_open());
    assert!(owner.drain().is_none());
}
