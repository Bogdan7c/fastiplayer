//! Слежение на настоящей файловой системе (tempdir): запись соседа будит цикл ровно один
//! раз, служебные файлы — нет, остановка завершает поток, поздний каталог подхватывается.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use atomic_file_store::{AtomicFileWriteOutcome, replace_file_atomically};

use super::*;
use crate::app_wake::{AppWakeEvent, AppWakeOwner, WakeEmitter};

/// Сколько ждать события файловой системы; inotify обычно доставляет за миллисекунды.
const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Пауза «точно больше ничего не придёт» после события.
const QUIET_PERIOD: Duration = Duration::from_millis(300);

/// Считает события пробуждения, которые дошли бы до цикла winit.
#[derive(Default)]
struct CountingEmitter {
    emitted: AtomicUsize,
}

impl WakeEmitter for CountingEmitter {
    fn emit(&self, event: AppWakeEvent) -> Result<(), ()> {
        assert_eq!(event.owner(), AppWakeOwner::ConfigFile);
        self.emitted.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Слежение за `config_file` с подсчётом пробуждений.
struct WatchHarness {
    watch: ConfigFileWatch,
    receiver: ConfigFileChangeReceiver,
    emitter: Arc<CountingEmitter>,
}

impl WatchHarness {
    fn start(config_file: &Path) -> Self {
        let emitter = Arc::new(CountingEmitter::default());
        let port = AppWakePort::new(AppWakeOwner::ConfigFile, emitter.clone());
        let (publisher, receiver) = config_file_change_signal(port);
        let ConfigFileWatchStart::Watching(watch) = ConfigFileWatch::start(config_file, publisher)
        else {
            panic!("inotify watch must start on Linux");
        };
        Self {
            watch,
            receiver,
            emitter,
        }
    }

    fn wakes(&self) -> usize {
        self.emitter.emitted.load(Ordering::SeqCst)
    }

    /// Ждёт, пока пробуждений станет не меньше `expected`.
    fn wait_for_wakes(&self, expected: usize) {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        while self.wakes() < expected {
            assert!(
                Instant::now() < deadline,
                "expected {expected} wakes, got {}",
                self.wakes()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Ждёт сигнал «сверься» после подписки и забирает его.
    fn consume_subscription_resync(&self) {
        self.wait_for_wakes(1);
        assert_eq!(
            self.receiver.take_change(),
            ConfigFileChange::MayHaveChanged
        );
    }
}

fn config_in(directory: &Path) -> PathBuf {
    directory.join("config.toml")
}

fn write_like_a_neighbour(config_file: &Path, contents: &str) {
    let outcome = replace_file_atomically(config_file, contents.as_bytes());
    assert!(
        !matches!(outcome, AtomicFileWriteOutcome::NotReplaced(_)),
        "atomic write must succeed: {outcome:?}"
    );
}

/// Атомарная запись соседа (temp + rename) даёт ровно один сигнал и одно пробуждение.
#[test]
fn neighbour_atomic_write_wakes_exactly_once() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config_file = config_in(directory.path());
    write_like_a_neighbour(&config_file, "schema_version = 1\n");
    let harness = WatchHarness::start(&config_file);
    harness.consume_subscription_resync();

    write_like_a_neighbour(&config_file, "schema_version = 2\n");
    harness.wait_for_wakes(2);
    std::thread::sleep(QUIET_PERIOD);

    assert_eq!(harness.wakes(), 2, "one write is one wake");
    assert_eq!(
        harness.receiver.take_change(),
        ConfigFileChange::MayHaveChanged
    );
    assert_eq!(harness.receiver.take_change(), ConfigFileChange::NoChange);
}

/// Замок записи и временные файлы с другим именем сигнала не дают.
#[test]
fn lock_and_temporary_files_do_not_signal() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config_file = config_in(directory.path());
    let harness = WatchHarness::start(&config_file);
    harness.consume_subscription_resync();

    fs::write(directory.path().join("config.toml.lock"), b"").expect("lock file");
    let temporary = directory.path().join(".config.toml.4242.0.tmp");
    fs::write(&temporary, b"draft").expect("temp file");
    fs::rename(&temporary, directory.path().join("other.toml")).expect("rename temp");
    fs::remove_file(directory.path().join("config.toml.lock")).expect("remove lock");
    std::thread::sleep(QUIET_PERIOD);

    assert_eq!(
        harness.wakes(),
        1,
        "only the subscription resync woke the loop"
    );
    assert_eq!(harness.receiver.take_change(), ConfigFileChange::NoChange);
}

/// Остановка завершает поток (join вернулся), и после неё записи цикл не будят.
#[test]
fn stop_joins_the_thread_and_silences_the_watch() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config_file = config_in(directory.path());
    let harness = WatchHarness::start(&config_file);
    harness.consume_subscription_resync();
    let WatchHarness {
        watch,
        receiver,
        emitter,
    } = harness;

    let started_stopping = Instant::now();
    assert_eq!(watch.stop(), ConfigFileWatchStopped::Stopped);
    assert!(
        started_stopping.elapsed() < EVENT_TIMEOUT,
        "the sleeping thread wakes on the stop channel at once"
    );

    write_like_a_neighbour(&config_file, "schema_version = 3\n");
    std::thread::sleep(QUIET_PERIOD);
    assert_eq!(emitter.emitted.load(Ordering::SeqCst), 1);
    assert_eq!(receiver.take_change(), ConfigFileChange::NoChange);
}

/// Каталога config-а ещё нет: слежение ждёт предка, а когда каталог появился и в нём
/// записан файл, сигнал приходит.
#[test]
fn directory_created_later_is_picked_up() {
    let home = tempfile::tempdir().expect("tempdir");
    let config_directory = home.path().join(".config").join("fastiplayer");
    let config_file = config_in(&config_directory);
    let harness = WatchHarness::start(&config_file);
    std::thread::sleep(QUIET_PERIOD);
    assert_eq!(
        harness.wakes(),
        0,
        "nothing to resync while the directory is missing"
    );

    fs::create_dir_all(&config_directory).expect("config directory");
    write_like_a_neighbour(&config_file, "schema_version = 1\n");
    harness.wait_for_wakes(1);
    assert_eq!(
        harness.receiver.take_change(),
        ConfigFileChange::MayHaveChanged
    );

    // Подписка теперь на самом каталоге: следующая запись снова будит цикл.
    let wakes_before = harness.wakes();
    write_like_a_neighbour(&config_file, "schema_version = 2\n");
    harness.wait_for_wakes(wakes_before + 1);
    assert_eq!(
        harness.receiver.take_change(),
        ConfigFileChange::MayHaveChanged
    );
}

/// Каталог удалили и создали заново: без паники, слежение возвращается к нему.
#[test]
fn removed_directory_is_watched_again_after_recreation() {
    let home = tempfile::tempdir().expect("tempdir");
    let config_directory = home.path().join("fastiplayer");
    fs::create_dir_all(&config_directory).expect("config directory");
    let config_file = config_in(&config_directory);
    let harness = WatchHarness::start(&config_file);
    harness.consume_subscription_resync();

    fs::remove_dir_all(&config_directory).expect("remove config directory");
    std::thread::sleep(QUIET_PERIOD);
    let _deletion_signal = harness.receiver.take_change();
    let wakes_before = harness.wakes();

    fs::create_dir_all(&config_directory).expect("recreate config directory");
    write_like_a_neighbour(&config_file, "schema_version = 1\n");
    harness.wait_for_wakes(wakes_before + 1);
    assert_eq!(
        harness.receiver.take_change(),
        ConfigFileChange::MayHaveChanged
    );
    assert_eq!(harness.watch.stop(), ConfigFileWatchStopped::Stopped);
}

/// Путь без каталога (каталог пользователя не найден) — понятный режим «не следим».
#[test]
fn path_without_directory_is_not_watched() {
    let emitter = Arc::new(CountingEmitter::default());
    let port = AppWakePort::new(AppWakeOwner::ConfigFile, emitter);
    let (publisher, _receiver) = config_file_change_signal(port);

    let start = ConfigFileWatch::start(Path::new(""), publisher);

    assert!(matches!(
        start,
        ConfigFileWatchStart::NotWatching(ConfigFileWatchUnavailable::ConfigDirectoryUnknown)
    ));
}
