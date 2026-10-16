//! Память выбора дорожек на границах жизненного цикла shell-а: запуск, suspend, выход.
//!
//! Память принадлежит процессу (`AppShell`), а не `AppState`: пересоздание `AppState` после
//! suspend её не теряет. Поэтому и запуск, и финальный сброс — здесь.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use fastiplayer_config::ConfigPaths;
use playlist_state::{TrackMemoryCapacity, TrackMemoryStore};

use crate::settings_runtime::SettingsRuntime;
use crate::state::{
    TrackMemoryHandle, TrackMemoryShutdownOutcome, TrackMemoryStartOutcome, start_track_memory,
};

use super::AppShell;

/// Сколько максимум ждём фоновую запись памяти при выходе: настройки выбора не стоят
/// зависшего закрытия окна.
const TRACK_MEMORY_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// Запускает память выбора по настройкам: файл рядом с config, ёмкость из `[playlist]`.
///
/// Ёмкость вне допустимого диапазона (config проверяет границы, но защищаемся) заменяется
/// максимально допустимой с предупреждением, а не отключает память.
pub(super) fn start_track_memory_from_config(
    config_paths: &ConfigPaths,
    configured_entries: u32,
) -> (TrackMemoryHandle, TrackMemoryStartOutcome) {
    let capacity = usize::try_from(configured_entries)
        .ok()
        .and_then(|entries| TrackMemoryCapacity::new(entries).ok())
        .unwrap_or_else(|| {
            tracing::warn!(
                configured_entries,
                "Track memory capacity is out of range; using the maximum"
            );
            TrackMemoryCapacity::HARD_MAX
        });
    start_track_memory(
        Arc::new(TrackMemoryStore::new(config_paths.track_memory_file())),
        capacity,
        SystemTime::now(),
    )
}

/// Запуск памяти для `AppShell::new`: исход уже записан в лог, оболочке нужен только доступ.
pub(super) fn start_for_shell(
    config_paths: &ConfigPaths,
    settings_runtime: &SettingsRuntime,
) -> TrackMemoryHandle {
    let (handle, _start_outcome) = start_track_memory_from_config(
        config_paths,
        settings_runtime
            .committed_config()
            .playlist
            .track_memory_max_entries,
    );
    handle
}

impl AppShell {
    /// Suspend: несохранённые выборы уходят на запись сразу (неблокирующе; поток записи
    /// переживает suspend вместе с процессом).
    pub(super) fn flush_track_memory_for_suspend(&mut self) {
        self.track_memory.lock().flush_unsaved_changes();
    }

    /// Выход: последний снимок и ожидание фоновой записи в пределах короткого бюджета.
    pub(super) fn shutdown_track_memory(&mut self) {
        let outcome = self
            .track_memory
            .lock()
            .shutdown(TRACK_MEMORY_SHUTDOWN_TIMEOUT);
        match outcome {
            TrackMemoryShutdownOutcome::NothingToFlush => {}
            TrackMemoryShutdownOutcome::Completed(report) => {
                tracing::debug!(?report, "Track memory flushed at exit");
            }
            TrackMemoryShutdownOutcome::TimedOut => {
                tracing::warn!(
                    "Track memory did not finish saving before exit; recent choices may be lost"
                );
            }
            TrackMemoryShutdownOutcome::WriterUnavailable => {
                tracing::warn!("Track memory writer was unavailable at exit");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::time::{Duration, Instant};

    use fastiplayer_config::ConfigPaths;
    use player_core::MediaInstanceId;
    use playlist_core::{LocalLocator, PlaylistLocator};
    use playlist_state::{RememberedSubtitleChoice, VideoKey};

    use super::start_track_memory_from_config;
    use crate::state::TrackMemoryStartOutcome;

    fn key_of(name: &str) -> VideoKey {
        VideoKey::for_locator(&PlaylistLocator::Local(LocalLocator::Native(name.into())))
            .expect("local key")
    }

    fn instance(raw: u64) -> MediaInstanceId {
        MediaInstanceId::from_non_zero(NonZeroU64::new(raw).expect("non-zero"))
    }

    #[test]
    fn capacity_from_config_limits_the_memory_and_evicts_the_oldest_video() {
        let directory = tempfile::tempdir().expect("tempdir");
        let paths = ConfigPaths::from_config_dir(directory.path());
        let (handle, outcome) = start_track_memory_from_config(&paths, 2);
        assert!(matches!(outcome, TrackMemoryStartOutcome::StartedEmpty));
        let now = Instant::now();
        let mut owner = handle.lock();

        for (raw, name) in [(1, "a.mkv"), (2, "b.mkv"), (3, "c.mkv")] {
            owner.begin_video(instance(raw), Some(key_of(name)), now);
            owner.remember_subtitles(instance(raw), RememberedSubtitleChoice::Off, now);
        }

        assert_eq!(owner.remembered_video_count(), 2);
        assert_eq!(owner.remembered_for_key(&key_of("a.mkv")).subtitles, None);
        assert!(
            owner
                .remembered_for_key(&key_of("c.mkv"))
                .subtitles
                .is_some()
        );
        owner.shutdown(Duration::from_secs(5));
        assert!(
            paths.track_memory_file().exists(),
            "the memory file lives next to the config"
        );
    }

    #[test]
    fn out_of_range_capacity_from_config_does_not_disable_the_memory() {
        let directory = tempfile::tempdir().expect("tempdir");
        let paths = ConfigPaths::from_config_dir(directory.path());

        let (handle, _) = start_track_memory_from_config(&paths, 0);

        assert!(handle.lock().capacity().entries() > 0);
    }
}
