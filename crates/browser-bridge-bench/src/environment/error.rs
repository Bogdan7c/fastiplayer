//! Ошибки изолированного окружения стенда.

use std::path::PathBuf;

/// Что пошло не так при создании, использовании или подключении окружения.
#[derive(Debug, thiserror::Error)]
pub enum EnvironmentError {
    /// Не удалось создать каталог, записать файл и т. п.
    #[error("{action}: {path}: {source}")]
    Io {
        /// Что делали («create directory», «write bus config»…).
        action: &'static str,
        /// К какому пути относилась операция.
        path: PathBuf,
        /// Исходная ошибка ОС.
        #[source]
        source: std::io::Error,
    },
    /// Не удалось запустить внешнюю программу (`dbus-daemon`, `kwin_wayland`…).
    #[error("cannot start {program}: {source} (is it installed?)")]
    Spawn {
        /// Имя программы.
        program: String,
        /// Исходная ошибка ОС.
        #[source]
        source: std::io::Error,
    },
    /// `dbus-daemon` не напечатал адрес шины.
    #[error("private dbus-daemon did not print its address (see {log})")]
    BusDidNotStart {
        /// Журнал шины (он же журнал плеера).
        log: PathBuf,
    },
    /// Невидимый экран не создал сокет Wayland за отведённое время.
    #[error("kwin_wayland did not create the Wayland socket {socket} (see {log})")]
    DisplayDidNotStart {
        /// Ожидаемый сокет.
        socket: PathBuf,
        /// Журнал kwin.
        log: PathBuf,
    },
    /// `kwin_wayland` завершился, не создав сокет: ждать бессмысленно.
    #[error("kwin_wayland exited early ({status}) before creating its Wayland socket (see {log})")]
    DisplayExited {
        /// Как завершился процесс («exit status: 1», «signal: 11»).
        status: String,
        /// Журнал kwin.
        log: PathBuf,
    },
    /// Режим `Real` без `WAYLAND_DISPLAY` в окружении процесса.
    #[error("DisplayTarget::Real needs WAYLAND_DISPLAY in the environment")]
    NoRealDisplay,
    /// Настоящий ввод разрешён на `Real`: он пустил бы синтетические клики на экран владельца.
    #[error("DesktopInputPolicy::Allowed is only valid with DisplayTarget::Virtual")]
    DesktopInputOnRealDisplay,
    /// Ввод или снимок запрошены, а окружение создано с `DesktopInputPolicy::Forbidden`
    /// (или это не невидимый экран): у kwin нет права отдавать их стенду.
    #[error("desktop input and screenshots are not enabled in this environment")]
    DesktopInputNotEnabled,
    /// Писатель настроек плеера отказал (невалидный или недоступный для записи файл).
    #[error("cannot write the player config {path}: {message}")]
    PlayerConfig {
        /// Куда писали.
        path: PathBuf,
        /// Текст ошибки писателя настроек.
        message: String,
    },
    /// Плеер или посредник не найдены там, где их ждёт установщик.
    #[error("player build is incomplete: {0}")]
    PlayerBuild(String),
    /// Установщик моста отказался подключить браузер.
    #[error("bridge installer failed: {0}")]
    Install(String),
}
