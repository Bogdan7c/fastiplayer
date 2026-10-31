//! Слежение за файлом config-а: «файл мог измениться» (решение владельца 136).
//!
//! Все копии плеера (обычное окно и окна браузерного режима) пишут один `config.toml`
//! («общая доска», решение 135). Чтобы копия увидела запись соседа без перезапуска, она
//! следит за **каталогом** config-а: запись идёт через временный файл и атомарный rename,
//! поэтому следить за самим файлом (его inode меняется при каждой записи) нельзя.
//!
//! Шов нейтральный: остальной код видит только
//! - [`ConfigFileWatch`] — живое слежение, которое останавливается при `Drop`/[`ConfigFileWatch::stop`];
//! - пару «издатель/приёмник» сигнала ([`config_file_change_signal`]): поток слежения
//!   поднимает флаг «файл мог измениться» и будит цикл winit через [`AppWakePort`];
//!   UI-поток забирает флаг и сам решает, что делать (перечитать, сравнить, применить).
//!
//! Что именно изменилось, слежение не знает и не читает файл: это дело владельца настроек.
//! Сигнал — только подсказка «посмотри»: ложный сигнал безвреден (своя запись даёт пустую
//! разницу), а пропущенный — нет, поэтому при сомнении (переполнение очереди событий,
//! новая подписка на каталог) сигнал поднимается.
//!
//! Реализации:
//! - Linux — своё поверх inotify (`linux.rs`): отдельный поток спит в `poll`, ноль
//!   периодических пробуждений, ноль новых зависимостей;
//! - остальные системы — честная заглушка «не следим» (`unsupported.rs`); под Windows
//!   сюда встанет своя реализация (`ReadDirectoryChangesW`) за тем же швом.

use std::io;
use std::path::Path;

use crate::app_wake::AppWakePort;

#[cfg(target_os = "linux")]
#[path = "config_file_watch/linux.rs"]
mod platform;
#[cfg(not(target_os = "linux"))]
#[path = "config_file_watch/unsupported.rs"]
mod platform;
mod signal;
#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;

pub(crate) use signal::{
    ConfigFileChange, ConfigFileChangePublisher, ConfigFileChangeReceiver,
    config_file_change_signal,
};

/// Почему слежения нет. Плеер при этом работает: чужие изменения настроек он увидит
/// после перезапуска.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigFileWatchUnavailable {
    /// На этой системе слежения пока нет (шов под будущие реализации).
    #[cfg_attr(
        target_os = "linux",
        expect(dead_code, reason = "строится только заглушкой не-Linux систем")
    )]
    UnsupportedPlatform,
    /// У пути config-а нет каталога (каталог пользователя не найден): следить не за чем.
    ConfigDirectoryUnknown,
    /// Системный ресурс слежения не создался.
    SetupFailed {
        /// На каком шаге.
        stage: ConfigFileWatchSetupStage,
        /// Безопасный класс ошибки ОС (без пути).
        error_kind: io::ErrorKind,
    },
}

/// Шаг подготовки слежения, на котором случилась ошибка ОС.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigFileWatchSetupStage {
    /// Создание очереди событий файловой системы (inotify).
    EventQueue,
    /// Создание канала остановки потока (eventfd).
    StopChannel,
    /// Запуск потока слежения.
    WatcherThread,
}

/// Итог запуска слежения.
pub(crate) enum ConfigFileWatchStart {
    /// Слежение идёт; живёт, пока жив возвращённый владелец.
    Watching(ConfigFileWatch),
    /// Слежения нет (причина — для журнала).
    NotWatching(ConfigFileWatchUnavailable),
}

/// Итог остановки слежения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigFileWatchStopped {
    /// Поток слежения завершился и присоединён.
    Stopped,
    /// Поток слежения завершился паникой (ошибка программы); ресурсы всё равно освобождены.
    WatcherThreadPanicked,
    /// Просьба остановиться не дошла до потока (ошибка ОС): поток отпущен и завершится
    /// вместе с процессом; ждать его нельзя — ожидание было бы вечным.
    StopRequestFailed,
}

/// Живое слежение за каталогом config-а; поток останавливается при `Drop`.
pub(crate) struct ConfigFileWatch {
    platform: platform::PlatformConfigWatch,
}

impl ConfigFileWatch {
    /// Начинает следить за каталогом `config_file` и поднимать сигнал через `publisher`.
    ///
    /// Сразу после подписки на каталог поднимается один сигнал «сверься»: между чтением
    /// config-а при старте и подпиской сосед мог успеть записать файл.
    pub(crate) fn start(
        config_file: &Path,
        publisher: ConfigFileChangePublisher,
    ) -> ConfigFileWatchStart {
        match platform::PlatformConfigWatch::start(config_file, publisher) {
            Ok(platform) => ConfigFileWatchStart::Watching(Self { platform }),
            Err(reason) => ConfigFileWatchStart::NotWatching(reason),
        }
    }

    /// Останавливает поток слежения и дожидается его завершения (в production то же
    /// делает `Drop`; явный исход нужен тестам).
    #[cfg(test)]
    pub(crate) fn stop(mut self) -> ConfigFileWatchStopped {
        self.platform.stop()
    }
}

impl Drop for ConfigFileWatch {
    fn drop(&mut self) {
        // Повторная остановка (после `stop`) — no-op внутри платформенной части; отказ
        // просьбы остановиться платформенная часть уже записала в журнал сама.
        if self.platform.stop() == ConfigFileWatchStopped::WatcherThreadPanicked {
            tracing::error!("Config file watcher thread panicked");
        }
    }
}

/// Запускает слежение и пишет в журнал одну строку, если слежения не будет.
///
/// Общая проводка для обычного окна и окна браузерного режима: оба окна получают одно и
/// то же поведение и один и тот же текст журнала.
pub(crate) fn start_config_file_watch(
    config_file: &Path,
    wake_port: AppWakePort,
) -> (Option<ConfigFileWatch>, ConfigFileChangeReceiver) {
    let (publisher, receiver) = config_file_change_signal(wake_port);
    let watch = match ConfigFileWatch::start(config_file, publisher) {
        ConfigFileWatchStart::Watching(watch) => Some(watch),
        ConfigFileWatchStart::NotWatching(reason) => {
            tracing::warn!(
                ?reason,
                "Settings file is not watched; changes from other Fastiplayer windows will \
                 apply after restart"
            );
            None
        }
    };
    (watch, receiver)
}
