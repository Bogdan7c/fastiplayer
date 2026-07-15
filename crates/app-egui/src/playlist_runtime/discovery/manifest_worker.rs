//! Bounded process-lifetime worker для deterministic directory manifest I/O.

use std::path::PathBuf;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use playlist_discovery::{
    DirectoryManifest, DirectoryManifestBuildError, build_directory_manifest,
};

use super::SiblingDiscoveryScopeId;
use crate::app_wake::AppWakePort;

/// Immutable команда одного manifest snapshot.
pub(super) struct ManifestWork {
    /// Scope нужен owner-у для отбрасывания late результата после cancel/supersede.
    pub(super) scope_id: SiblingDiscoveryScopeId,
    /// Original explicit locator; canonical path не подменяет source identity.
    pub(super) target_path: PathBuf,
}

/// Результат filesystem stage с exact scope correlation.
pub(super) struct ManifestWorkResult {
    /// Scope, которому принадлежит manifest.
    pub(super) scope_id: SiblingDiscoveryScopeId,
    /// Полный deterministic manifest либо typed atomic failure без batch prefix.
    pub(super) result: Result<DirectoryManifest, DirectoryManifestBuildError>,
}

/// Один reusable worker отделяет blocking directory I/O от UI/controller owner-а.
pub(super) struct ManifestWorker {
    /// Bounded ingress не допускает unbounded pending scans.
    sender: Option<mpsc::SyncSender<ManifestWork>>,
    /// Bounded result channel хранит завершения до event-driven UI drain.
    pub(super) receiver: mpsc::Receiver<ManifestWorkResult>,
    /// Process owner сохраняет join authority до terminal drop.
    join_handle: Option<JoinHandle<()>>,
}

impl ManifestWorker {
    /// Запускает единственный process-lifetime filesystem owner.
    pub(super) fn start(wake_port: AppWakePort) -> Option<Self> {
        let (sender, work_receiver) = mpsc::sync_channel::<ManifestWork>(1);
        let (result_sender, receiver) = mpsc::sync_channel::<ManifestWorkResult>(2);
        let join_handle = thread::Builder::new()
            .name("playlist-manifest".to_owned())
            .spawn(move || {
                while let Ok(work) = work_receiver.recv() {
                    let result = build_directory_manifest(&work.target_path);
                    if result_sender
                        .send(ManifestWorkResult {
                            scope_id: work.scope_id,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    let _wake_delivery = wake_port.request_wake();
                }
            })
            .ok()?;
        Some(Self {
            sender: Some(sender),
            receiver,
            join_handle: Some(join_handle),
        })
    }

    /// Bounded try-send сохраняет UI thread non-blocking даже при занятом worker-е.
    pub(super) fn submit(&self, work: ManifestWork) -> Result<(), ()> {
        self.sender
            .as_ref()
            .ok_or(())?
            .try_send(work)
            .map_err(|_| ())
    }

    /// Закрывает admission без ожидания потенциально blocking filesystem syscall-а.
    pub(super) fn close_admission(&mut self) {
        self.sender = None;
    }
}

impl Drop for ManifestWorker {
    fn drop(&mut self) {
        self.sender = None;
        if let Some(join_handle) = self.join_handle.take() {
            let _join_result = join_handle.join();
        }
    }
}
