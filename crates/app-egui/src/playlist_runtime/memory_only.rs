//! Runtime очереди «только в памяти» для окна браузерного режима (мост, сессия 04a).
//!
//! Окно браузерного режима собирается из тех же деталей, что обычное (решение 120), и
//! `AppState` открывает медиа через координатор открытия, который живёт в
//! [`PlaylistRuntime`]. Но очередь там пустая и ничего не пишет на диск (инвариант 7):
//! - хранилища состояния очереди нет — проверка файла при старте не нужна, поэтому
//!   решение «можно грузить» принимается сразу, без фонового чтения диска;
//! - хранилища позиции возобновления нет, и само возобновление выключено;
//! - MPRIS обычного плеера (`start_desktop_transport`) не запускается: у браузерного
//!   режима будет своё имя (04c).

use fastiplayer_config::PlaylistConfig;

use super::{PlaylistLineagePersistence, PlaylistRuntime};
use crate::app_wake::AppWakePort;

impl PlaylistRuntime {
    /// Пустая очередь без файлов: готова к открытию медиа сразу после создания.
    ///
    /// «Постоянная линия» здесь значит только «запись не запрещена политикой», а
    /// писать некуда: без установленного хранилища писатель очереди не создаётся
    /// (`persistence::start_for_lineage`), а возобновление без хранилища закрыто
    /// (`ResumeStoreAccess::Uninitialized`) и вдобавок выключено.
    ///
    /// # Errors
    /// Контроллер пустой очереди не собрался из `[playlist]` (ошибка сборки).
    pub(crate) fn new_memory_only(
        wake_port: AppWakePort,
        playlist_config: PlaylistConfig,
    ) -> Result<Self, super::PlaylistStartupApplyError> {
        let mut runtime = Self::new_with_resume_policy(
            wake_port,
            playlist_config,
            // resume_last_position: позицию браузерного потока не запоминаем.
            false,
        );
        runtime.complete_new_lineage(PlaylistLineagePersistence::Persistent, None)?;
        Ok(runtime)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_wake::AppWakeOwner;

    #[test]
    fn memory_only_runtime_is_open_for_media_without_any_disk_owner() {
        let mut runtime = PlaylistRuntime::new_memory_only(
            AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime),
            PlaylistConfig::default(),
        )
        .expect("empty queue");

        assert!(
            runtime.allocator_load_gate_is_open(),
            "media opens without waiting for a disk inspection"
        );
        assert!(
            !runtime.has_pending_playlist_persistence_work(),
            "no startup inspection and no save worker"
        );
        let binding = runtime
            .bind_resumed_app_state()
            .expect("AppState can bind to the runtime");
        let attachment = runtime
            .app_state_attachment(binding)
            .expect("attachment for the window AppState");
        assert_eq!(attachment.binding(), binding);
        // Выход: писателя очереди не было вовсе, сохранять нечего.
        let shutdown = runtime.shutdown_until(crate::process_shutdown::ShutdownDeadline::after(
            std::time::Duration::from_secs(2),
        ));
        let super::super::PlaylistTerminalShutdownOutcome::Completed(report) = shutdown else {
            panic!("clean shutdown expected, got {shutdown:?}");
        };
        assert!(
            matches!(
                report.persistence,
                super::super::persistence::PlaylistPersistenceShutdownOutcome::CompletedWithoutWorker {
                    save_block: None
                }
            ),
            "the queue had no writer: {:?}",
            report.persistence
        );
    }
}
