//! Слежение за каталогом config-а на Linux поверх inotify (`man 7 inotify`).
//!
//! Поток слежения спит в `poll` на двух дескрипторах: очередь inotify и eventfd
//! остановки. Таймаута нет — ноль периодических пробуждений: поток просыпается только от
//! события файловой системы или от просьбы остановиться.
//!
//! Подписка:
//! - каталог config-а есть — следим за ним (`IN_CLOSE_WRITE`, `IN_MOVED_TO`, `IN_CREATE`,
//!   `IN_DELETE`, `IN_MOVED_FROM`) и поднимаем сигнал только для событий с именем файла
//!   config-а. Замок записи (`config.toml.lock`) и временные файлы атомарной записи имеют
//!   другие имена и игнорируются: атомарная запись соседа даёт ровно одно событие
//!   `IN_MOVED_TO config.toml`;
//! - каталога ещё нет (чистый HOME, браузерный процесс до первой записи) — следим за
//!   ближайшим существующим предком и ждём, когда в нём появится следующая часть пути.
//!   Как только она появилась, подписка спускается ниже, вплоть до каталога config-а.
//!   Выбран этот способ, а не «переподписаться после своей первой записи»: каталог может
//!   создать и соседняя копия, и человек;
//! - каталог удалён или переименован (`IN_DELETE_SELF`, `IN_MOVE_SELF`, `IN_IGNORED`) —
//!   без паники возвращаемся к ожиданию предка.
//!
//! После каждой подписки на сам каталог config-а поднимается сигнал «сверься»: файл мог
//! появиться или измениться, пока подписки не было.
//!
//! Ошибки ОС при подписке (лимит `max_user_watches`, нет прав) — одно предупреждение в
//! журнал, и поток завершается: плеер работает дальше без слежения.

use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

use super::{
    ConfigFileChangePublisher, ConfigFileWatchSetupStage, ConfigFileWatchStopped,
    ConfigFileWatchUnavailable,
};

/// События каталога config-а, которые могут означать новое содержимое файла.
const CONFIG_FILE_EVENTS: u32 = libc::IN_CLOSE_WRITE
    | libc::IN_MOVED_TO
    | libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MOVED_FROM;

/// События «наблюдаемый каталог исчез» (удалён, переименован, подписка снята ядром).
const WATCHED_DIRECTORY_GONE: u32 = libc::IN_DELETE_SELF | libc::IN_MOVE_SELF | libc::IN_IGNORED;

/// Маска подписки на сам каталог config-а.
///
/// `IN_ONLYDIR` — подписка только на каталог (файл с тем же именем не подойдёт);
/// `IN_EXCL_UNLINK` — не получать событий от уже удалённых имён.
const CONFIG_DIRECTORY_MASK: u32 = CONFIG_FILE_EVENTS
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_ONLYDIR
    | libc::IN_EXCL_UNLINK;

/// Маска подписки на предка, пока каталога config-а нет: ждём появления следующей части пути.
const ANCESTOR_MASK: u32 = libc::IN_CREATE
    | libc::IN_MOVED_TO
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_ONLYDIR
    | libc::IN_EXCL_UNLINK;

/// Размер заголовка `struct inotify_event` без имени: wd, mask, cookie, len (по 4 байта).
const INOTIFY_EVENT_HEADER_LEN: usize = 16;

/// Буфер чтения очереди: man 7 inotify советует не меньше `sizeof(inotify_event) + NAME_MAX + 1`
/// (16 + 255 + 1); 4 КиБ вмещают пачку событий за одно чтение.
const INOTIFY_READ_BUFFER_LEN: usize = 4096;

/// Платформенная часть [`super::ConfigFileWatch`].
pub(super) struct PlatformConfigWatch {
    /// Конец eventfd остановки у владельца (у потока — своя копия дескриптора).
    stop_channel: File,
    /// Поток слежения; `None` после остановки.
    thread: Option<JoinHandle<()>>,
}

impl PlatformConfigWatch {
    /// Создаёт очередь inotify, канал остановки и поток слежения.
    pub(super) fn start(
        config_file: &Path,
        publisher: ConfigFileChangePublisher,
    ) -> Result<Self, ConfigFileWatchUnavailable> {
        let target = WatchedConfigPath::from_config_file(config_file)
            .ok_or(ConfigFileWatchUnavailable::ConfigDirectoryUnknown)?;
        let event_queue = new_inotify_queue()
            .map_err(|error| setup_failed(ConfigFileWatchSetupStage::EventQueue, &error))?;
        let stop_channel = new_stop_eventfd()
            .map_err(|error| setup_failed(ConfigFileWatchSetupStage::StopChannel, &error))?;
        let thread_stop_channel = stop_channel
            .try_clone()
            .map_err(|error| setup_failed(ConfigFileWatchSetupStage::StopChannel, &error))?;
        let thread = std::thread::Builder::new()
            .name("config-file-watch".to_owned())
            .spawn(move || {
                let mut watcher = DirectoryWatcher {
                    event_queue,
                    stop_channel: thread_stop_channel,
                    target,
                    publisher,
                    subscription: None,
                };
                watcher.run();
            })
            .map_err(|error| setup_failed(ConfigFileWatchSetupStage::WatcherThread, &error))?;
        Ok(Self {
            stop_channel,
            thread: Some(thread),
        })
    }

    /// Просит поток остановиться и ждёт его; повторный вызов — no-op.
    pub(super) fn stop(&mut self) -> ConfigFileWatchStopped {
        let Some(thread) = self.thread.take() else {
            return ConfigFileWatchStopped::Stopped;
        };
        // Любое ненулевое значение в eventfd делает его читаемым и будит `poll` потока.
        if let Err(error) = (&self.stop_channel).write_all(&1_u64.to_ne_bytes()) {
            // Без сигнала остановки join повис бы навсегда: поток отпускаем (он умрёт с
            // процессом), а вызывающий получает отдельный исход.
            tracing::error!(kind = ?error.kind(), "Config file watcher could not be asked to stop");
            return ConfigFileWatchStopped::StopRequestFailed;
        }
        match thread.join() {
            Ok(()) => ConfigFileWatchStopped::Stopped,
            Err(_panic_payload) => ConfigFileWatchStopped::WatcherThreadPanicked,
        }
    }
}

fn setup_failed(stage: ConfigFileWatchSetupStage, error: &io::Error) -> ConfigFileWatchUnavailable {
    ConfigFileWatchUnavailable::SetupFailed {
        stage,
        error_kind: error.kind(),
    }
}

/// Каталог config-а и имя файла в нём.
struct WatchedConfigPath {
    directory: PathBuf,
    file_name: OsString,
}

impl WatchedConfigPath {
    /// `None` — у пути нет каталога или имени (например, пустой путь: каталог пользователя
    /// не найден), следить не за чем.
    fn from_config_file(config_file: &Path) -> Option<Self> {
        let file_name = config_file.file_name()?.to_owned();
        let directory = config_file.parent()?;
        if directory.as_os_str().is_empty() {
            return None;
        }
        Some(Self {
            directory: directory.to_path_buf(),
            file_name,
        })
    }
}

/// Текущая подписка потока.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Subscription {
    /// Следим за каталогом config-а.
    ConfigDirectory { watch: WatchDescriptor },
    /// Каталога нет: следим за предком и ждём появления `awaited_child` в нём.
    Ancestor {
        watch: WatchDescriptor,
        awaited_child: OsString,
    },
}

impl Subscription {
    fn watch(&self) -> WatchDescriptor {
        match self {
            Self::ConfigDirectory { watch } | Self::Ancestor { watch, .. } => *watch,
        }
    }
}

/// Номер подписки inotify (`wd`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WatchDescriptor(i32);

/// Почему поток прекратил слежение.
enum WatchLoopExit {
    /// Владелец попросил остановиться.
    StopRequested,
    /// Ошибка ОС: дальше без слежения.
    Failed {
        operation: &'static str,
        error_kind: io::ErrorKind,
    },
}

/// Что сделать после разбора пачки событий.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct EventBatchDecision {
    /// Поднять сигнал «файл мог измениться».
    signal_change: bool,
    /// Подписка устарела (каталог исчез или появился): подписаться заново.
    resubscribe: bool,
}

/// Состояние потока слежения.
struct DirectoryWatcher {
    event_queue: File,
    stop_channel: File,
    target: WatchedConfigPath,
    publisher: ConfigFileChangePublisher,
    subscription: Option<Subscription>,
}

impl DirectoryWatcher {
    /// Тело потока: подписка, затем ожидание событий до просьбы остановиться.
    fn run(&mut self) {
        let exit = self.watch_until_stopped();
        if let WatchLoopExit::Failed {
            operation,
            error_kind,
        } = exit
        {
            // Ровно одна строка за жизнь слежения: после неё поток завершается.
            tracing::warn!(
                operation,
                ?error_kind,
                "Settings file watch stopped; changes from other Fastiplayer windows will \
                 apply after restart"
            );
        }
    }

    fn watch_until_stopped(&mut self) -> WatchLoopExit {
        if let Err(exit) = self.resubscribe() {
            return exit;
        }
        loop {
            match self.wait_for_readiness() {
                Ok(Readiness::StopRequested) => return WatchLoopExit::StopRequested,
                Ok(Readiness::EventsAvailable) => {}
                Err(exit) => return exit,
            }
            let decision = match self.drain_events() {
                Ok(decision) => decision,
                Err(exit) => return exit,
            };
            if decision.resubscribe
                && let Err(exit) = self.resubscribe()
            {
                return exit;
            }
            if decision.signal_change {
                let _delivery = self.publisher.publish_may_have_changed();
            }
        }
    }

    /// Ждёт событие inotify или просьбу остановиться (без таймаута).
    fn wait_for_readiness(&self) -> Result<Readiness, WatchLoopExit> {
        let mut descriptors = [
            libc::pollfd {
                fd: self.event_queue.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.stop_channel.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        loop {
            // SAFETY: `descriptors` — живой массив из двух `pollfd` на время вызова, длина
            // передана точно; оба fd принадлежат живым `File` этого потока. Таймаут -1 —
            // ждать без ограничения (ноль периодических пробуждений).
            let ready = unsafe {
                libc::poll(
                    descriptors.as_mut_ptr(),
                    descriptors.len() as libc::nfds_t,
                    -1,
                )
            };
            if ready >= 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(WatchLoopExit::Failed {
                    operation: "poll",
                    error_kind: error.kind(),
                });
            }
        }
        let [queue, stop] = descriptors;
        if stop.revents != 0 {
            return Ok(Readiness::StopRequested);
        }
        if queue.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(WatchLoopExit::Failed {
                operation: "poll inotify",
                error_kind: io::ErrorKind::BrokenPipe,
            });
        }
        Ok(Readiness::EventsAvailable)
    }

    /// Вычитывает очередь до дна и решает, что делать с пачкой.
    fn drain_events(&mut self) -> Result<EventBatchDecision, WatchLoopExit> {
        let mut decision = EventBatchDecision::default();
        let mut buffer = [0_u8; INOTIFY_READ_BUFFER_LEN];
        loop {
            let read_len = match (&self.event_queue).read(&mut buffer) {
                Ok(read_len) => read_len,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    return Err(WatchLoopExit::Failed {
                        operation: "read inotify",
                        error_kind: error.kind(),
                    });
                }
            };
            if read_len == 0 {
                break;
            }
            for event in InotifyEvents::new(&buffer[..read_len]) {
                let event_decision =
                    classify_event(&event, self.subscription.as_ref(), &self.target);
                decision.signal_change |= event_decision.signal_change;
                decision.resubscribe |= event_decision.resubscribe;
            }
        }
        Ok(decision)
    }

    /// Снимает старую подписку и подписывается заново, спускаясь к каталогу config-а.
    fn resubscribe(&mut self) -> Result<(), WatchLoopExit> {
        if let Some(previous) = self.subscription.take() {
            // Ядро могло уже снять подписку само (каталог удалён): EINVAL здесь ожидаем.
            remove_watch(&self.event_queue, previous.watch());
        }
        let subscription = subscribe(&self.event_queue, &self.target)?;
        let reached_config_directory = matches!(subscription, Subscription::ConfigDirectory { .. });
        self.subscription = Some(subscription);
        if reached_config_directory {
            // Пока подписки не было, файл мог появиться или измениться: «сверься».
            let _delivery = self.publisher.publish_may_have_changed();
        }
        Ok(())
    }
}

/// Готовность дескрипторов после `poll`.
enum Readiness {
    StopRequested,
    EventsAvailable,
}

/// Одно разобранное событие inotify.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InotifyEvent<'buffer> {
    watch: WatchDescriptor,
    mask: u32,
    /// Имя внутри каталога (без завершающих NUL); пусто — событие самого каталога.
    name: &'buffer [u8],
}

/// Разбор буфера `read(inotify_fd)` без `unsafe`: заголовок читается побайтно, поэтому
/// выравнивание буфера не важно.
struct InotifyEvents<'buffer> {
    remaining: &'buffer [u8],
}

impl<'buffer> InotifyEvents<'buffer> {
    fn new(buffer: &'buffer [u8]) -> Self {
        Self { remaining: buffer }
    }
}

impl<'buffer> Iterator for InotifyEvents<'buffer> {
    type Item = InotifyEvent<'buffer>;

    fn next(&mut self) -> Option<Self::Item> {
        let header = self.remaining.get(..INOTIFY_EVENT_HEADER_LEN)?;
        let field =
            |offset: usize| -> Option<[u8; 4]> { header.get(offset..offset + 4)?.try_into().ok() };
        let watch = i32::from_ne_bytes(field(0)?);
        let mask = u32::from_ne_bytes(field(4)?);
        let name_len = usize::try_from(u32::from_ne_bytes(field(12)?)).ok()?;
        let event_end = INOTIFY_EVENT_HEADER_LEN.checked_add(name_len)?;
        // Ядро всегда отдаёт целые события; обрезанный хвост означает ошибку — стоп.
        let padded_name = self.remaining.get(INOTIFY_EVENT_HEADER_LEN..event_end)?;
        self.remaining = self.remaining.get(event_end..)?;
        let name_end = padded_name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(padded_name.len());
        Some(InotifyEvent {
            watch: WatchDescriptor(watch),
            mask,
            name: padded_name.get(..name_end)?,
        })
    }
}

/// Решение по одному событию при данной подписке (чистая функция — проверяется тестами).
fn classify_event(
    event: &InotifyEvent<'_>,
    subscription: Option<&Subscription>,
    target: &WatchedConfigPath,
) -> EventBatchDecision {
    if event.mask & libc::IN_Q_OVERFLOW != 0 {
        // События потеряны: файл мог измениться, а предок — получить нужный каталог.
        return EventBatchDecision {
            signal_change: true,
            resubscribe: true,
        };
    }
    let Some(subscription) = subscription else {
        return EventBatchDecision::default();
    };
    if event.watch != subscription.watch() {
        // Хвост событий старой подписки (например, IN_IGNORED после переподписки).
        return EventBatchDecision::default();
    }
    if event.mask & WATCHED_DIRECTORY_GONE != 0 {
        return EventBatchDecision {
            signal_change: false,
            resubscribe: true,
        };
    }
    match subscription {
        Subscription::ConfigDirectory { .. } => EventBatchDecision {
            signal_change: event.mask & CONFIG_FILE_EVENTS != 0
                && event.name == target.file_name.as_bytes(),
            resubscribe: false,
        },
        Subscription::Ancestor { awaited_child, .. } => EventBatchDecision {
            signal_change: false,
            resubscribe: event.mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0
                && event.name == awaited_child.as_bytes(),
        },
    }
}

/// Подписывается на каталог config-а, а если его нет — на ближайшего существующего предка.
fn subscribe(
    event_queue: &File,
    target: &WatchedConfigPath,
) -> Result<Subscription, WatchLoopExit> {
    // Каждый круг либо подписывается, либо поднимается на уровень выше, либо (гонка:
    // нужный каталог появился между попытками) начинает спуск заново. Число спусков
    // ограничено: бесконечное создание/удаление каталогов не должно держать поток.
    const MAX_SUBSCRIBE_ROUNDS: usize = 64;
    for _round in 0..MAX_SUBSCRIBE_ROUNDS {
        match add_watch(event_queue, &target.directory, CONFIG_DIRECTORY_MASK) {
            Ok(watch) => return Ok(Subscription::ConfigDirectory { watch }),
            Err(error) if is_missing_directory(&error) => {}
            Err(error) => return Err(add_watch_failed(&error)),
        }
        match subscribe_to_nearest_ancestor(event_queue, &target.directory)? {
            AncestorSubscription::Waiting(subscription) => return Ok(subscription),
            AncestorSubscription::ChildAppearedMeanwhile => {}
        }
    }
    Err(WatchLoopExit::Failed {
        operation: "subscribe to config directory",
        error_kind: io::ErrorKind::Other,
    })
}

/// Итог подписки на предка.
enum AncestorSubscription {
    /// Подписка стоит, ждём появления следующей части пути.
    Waiting(Subscription),
    /// Пока подписывались, нужный каталог появился: начать спуск заново.
    ChildAppearedMeanwhile,
}

fn subscribe_to_nearest_ancestor(
    event_queue: &File,
    config_directory: &Path,
) -> Result<AncestorSubscription, WatchLoopExit> {
    let mut missing_directory = config_directory;
    loop {
        let (Some(parent), Some(awaited_child)) =
            (missing_directory.parent(), missing_directory.file_name())
        else {
            // Дошли до корня, а он «не существует» — следить не за чем.
            return Err(WatchLoopExit::Failed {
                operation: "find an existing parent of the config directory",
                error_kind: io::ErrorKind::NotFound,
            });
        };
        match add_watch(event_queue, parent, ANCESTOR_MASK) {
            Ok(watch) => {
                // Гонка: каталог мог появиться между неудачной попыткой и подпиской на
                // предка — тогда события о нём уже не будет.
                if parent.join(awaited_child).is_dir() {
                    remove_watch(event_queue, watch);
                    return Ok(AncestorSubscription::ChildAppearedMeanwhile);
                }
                return Ok(AncestorSubscription::Waiting(Subscription::Ancestor {
                    watch,
                    awaited_child: awaited_child.to_owned(),
                }));
            }
            Err(error) if is_missing_directory(&error) => missing_directory = parent,
            Err(error) => return Err(add_watch_failed(&error)),
        }
    }
}

/// ENOENT — каталога нет; ENOTDIR — на его месте файл или нет промежуточного каталога.
fn is_missing_directory(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ENOTDIR))
}

fn add_watch_failed(error: &io::Error) -> WatchLoopExit {
    WatchLoopExit::Failed {
        // ENOSPC здесь — лимит `fs.inotify.max_user_watches`.
        operation: "inotify_add_watch",
        error_kind: error.kind(),
    }
}

/// Создаёт неблокирующую очередь inotify с `CLOEXEC`.
fn new_inotify_queue() -> io::Result<File> {
    // SAFETY: `inotify_init1` не принимает указателей и возвращает новый fd или -1.
    let descriptor = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    owned_file_from_descriptor(descriptor)
}

/// Создаёт неблокирующий eventfd с `CLOEXEC` для просьбы остановиться.
fn new_stop_eventfd() -> io::Result<File> {
    // SAFETY: `eventfd` не принимает указателей и возвращает новый fd или -1.
    let descriptor = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
    owned_file_from_descriptor(descriptor)
}

/// Забирает свежий fd во владение `File` (закроется сам).
fn owned_file_from_descriptor(descriptor: libc::c_int) -> io::Result<File> {
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `descriptor` только что вернул системный вызов, он валиден и больше никем не
    // владеется; `OwnedFd` закроет его ровно один раз.
    let owned = unsafe { OwnedFd::from_raw_fd(descriptor) };
    Ok(File::from(owned))
}

fn add_watch(event_queue: &File, directory: &Path, mask: u32) -> io::Result<WatchDescriptor> {
    let c_path = path_to_c_string(directory.as_os_str())?;
    // SAFETY: `c_path` — живая NUL-терминированная строка на время вызова; fd очереди
    // принадлежит живому `File`.
    let watch = unsafe { libc::inotify_add_watch(event_queue.as_raw_fd(), c_path.as_ptr(), mask) };
    if watch < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(WatchDescriptor(watch))
}

fn remove_watch(event_queue: &File, watch: WatchDescriptor) {
    // SAFETY: вызов принимает только целые числа; fd очереди принадлежит живому `File`.
    let result = unsafe { libc::inotify_rm_watch(event_queue.as_raw_fd(), watch.0) };
    if result != 0 {
        let error = io::Error::last_os_error();
        // EINVAL — ядро уже сняло подписку (каталог удалён): это ожидаемо.
        if error.raw_os_error() != Some(libc::EINVAL) {
            tracing::debug!(kind = ?error.kind(), "inotify_rm_watch failed");
        }
    }
}

fn path_to_c_string(path: &OsStr) -> io::Result<CString> {
    CString::new(path.as_bytes()).map_err(|_interior_nul| {
        io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte")
    })
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    /// Собирает событие в том виде, в каком его отдаёт ядро (имя дополнено NUL до 16).
    fn raw_event(watch: i32, mask: u32, name: &str) -> Vec<u8> {
        let mut padded_name = name.as_bytes().to_vec();
        if !padded_name.is_empty() {
            padded_name.resize((padded_name.len() / 16 + 1) * 16, 0);
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&watch.to_ne_bytes());
        bytes.extend_from_slice(&mask.to_ne_bytes());
        bytes.extend_from_slice(&0_u32.to_ne_bytes());
        bytes.extend_from_slice(&(padded_name.len() as u32).to_ne_bytes());
        bytes.extend_from_slice(&padded_name);
        bytes
    }

    fn target() -> WatchedConfigPath {
        WatchedConfigPath::from_config_file(Path::new("/home/user/.config/fastiplayer/config.toml"))
            .expect("absolute path has a directory")
    }

    #[test]
    fn parser_reads_a_batch_of_events_with_padded_names() {
        let mut batch = raw_event(3, libc::IN_CREATE, ".config.toml.12.0.tmp");
        batch.extend(raw_event(3, libc::IN_MOVED_TO, "config.toml"));
        batch.extend(raw_event(3, libc::IN_IGNORED, ""));

        let events: Vec<_> = InotifyEvents::new(&batch).collect();

        assert_eq!(events.len(), 3);
        assert_eq!(events[0].name, b".config.toml.12.0.tmp");
        assert_eq!(events[1].name, b"config.toml");
        assert_eq!(events[1].mask, libc::IN_MOVED_TO);
        assert!(events[2].name.is_empty());
    }

    #[test]
    fn truncated_tail_is_not_parsed_as_an_event() {
        let batch = raw_event(3, libc::IN_MOVED_TO, "config.toml");

        let events: Vec<_> = InotifyEvents::new(&batch[..batch.len() - 1]).collect();

        assert!(events.is_empty());
    }

    #[test]
    fn only_config_file_events_signal_in_the_config_directory() {
        let subscription = Subscription::ConfigDirectory {
            watch: WatchDescriptor(3),
        };
        let decide = |mask: u32, name: &'static str| {
            classify_event(
                &InotifyEvent {
                    watch: WatchDescriptor(3),
                    mask,
                    name: name.as_bytes(),
                },
                Some(&subscription),
                &target(),
            )
        };

        assert!(decide(libc::IN_MOVED_TO, "config.toml").signal_change);
        assert!(decide(libc::IN_CLOSE_WRITE, "config.toml").signal_change);
        assert!(!decide(libc::IN_CLOSE_WRITE, "config.toml.lock").signal_change);
        assert!(!decide(libc::IN_MOVED_FROM, ".config.toml.7.1.tmp").signal_change);
        assert!(decide(libc::IN_DELETE_SELF, "").resubscribe);
    }

    #[test]
    fn ancestor_waits_for_its_child_and_ignores_stale_watches() {
        let subscription = Subscription::Ancestor {
            watch: WatchDescriptor(5),
            awaited_child: OsString::from("fastiplayer"),
        };
        let event = |watch: i32, name: &'static str| InotifyEvent {
            watch: WatchDescriptor(watch),
            mask: libc::IN_CREATE | libc::IN_ISDIR,
            name: name.as_bytes(),
        };

        let child = classify_event(&event(5, "fastiplayer"), Some(&subscription), &target());
        let other = classify_event(&event(5, "other-app"), Some(&subscription), &target());
        let stale = classify_event(&event(4, "fastiplayer"), Some(&subscription), &target());

        assert_eq!(
            child,
            EventBatchDecision {
                signal_change: false,
                resubscribe: true
            }
        );
        assert_eq!(other, EventBatchDecision::default());
        assert_eq!(stale, EventBatchDecision::default());
    }

    #[test]
    fn empty_or_bare_paths_are_not_watchable() {
        assert!(WatchedConfigPath::from_config_file(Path::new("")).is_none());
        assert!(WatchedConfigPath::from_config_file(Path::new("config.toml")).is_none());
    }
}
