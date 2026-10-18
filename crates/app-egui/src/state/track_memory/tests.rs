//! Тесты памяти выбора на стороне приложения: владелец, запись на диск, ключ, звук.
//! Сквозная цепочка «выбор -> файл -> перезапуск -> звук и субтитры на кадре» —
//! в `state/subtitles/tests/memory_chain.rs` (нужны фикстуры и владелец субтитров).

mod audio_plan;
mod identity;
mod owner_flow;
mod startup_files;
mod web_audio_language;
mod wiring_guards;

use std::io::Write;
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use player_core::MediaInstanceId;
use playlist_core::{LocalLocator, PlaylistLocator};
use playlist_state::{RememberedTrackHint, TrackMemoryCapacity, TrackMemoryStore, VideoKey};

use super::{TrackMemoryHandle, TrackMemoryStartOutcome, start_track_memory};

pub(super) fn instance(raw: u64) -> MediaInstanceId {
    MediaInstanceId::from_non_zero(NonZeroU64::new(raw).expect("non-zero test id"))
}

pub(super) fn capacity(entries: usize) -> TrackMemoryCapacity {
    TrackMemoryCapacity::new(entries).expect("valid capacity")
}

pub(super) fn memory_file(directory: &Path) -> std::path::PathBuf {
    directory.join(playlist_state::TRACK_MEMORY_FILENAME)
}

/// Запускает память в каталоге так же, как `AppShell` при старте процесса.
pub(super) fn start_in(
    directory: &Path,
    entries: usize,
) -> (TrackMemoryHandle, TrackMemoryStartOutcome) {
    start_track_memory(
        Arc::new(TrackMemoryStore::new(memory_file(directory))),
        capacity(entries),
        SystemTime::now(),
    )
}

/// Ключ локального видео с таким именем.
pub(super) fn key_of(name: &str) -> VideoKey {
    VideoKey::for_locator(&PlaylistLocator::Local(LocalLocator::Native(name.into())))
        .expect("local key")
}

pub(super) fn audio_hint(
    language: &str,
    title: &str,
    codec: &str,
    ordinal: u32,
) -> RememberedTrackHint {
    RememberedTrackHint::new(
        Some(language.to_owned()),
        Some(title.to_owned()),
        codec.to_owned(),
        ordinal,
    )
    .expect("valid hint")
}

/// Ждёт, пока в файле памяти появится запись для `key` (фоновая запись асинхронна).
pub(super) fn wait_for_entry_on_disk(
    directory: &Path,
    key: &VideoKey,
) -> playlist_state::TrackMemory {
    let store = TrackMemoryStore::new(memory_file(directory));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let playlist_state::TrackMemoryInspection::Loaded(memory) = store.inspect(capacity(100))
            && memory.entry_for(key).is_some()
        {
            return memory;
        }
        assert!(
            Instant::now() < deadline,
            "the entry never reached the disk"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// `Write`, который копит вывод subscriber-а в общий буфер теста.
#[derive(Clone, Default)]
pub(super) struct CapturedLog(Arc<Mutex<Vec<u8>>>);

impl Write for CapturedLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| std::io::Error::other("poisoned log buffer"))?
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl CapturedLog {
    pub(super) fn text(&self) -> String {
        String::from_utf8(self.0.lock().expect("log buffer").clone()).expect("UTF-8 log")
    }
}

/// Выполняет `action` и возвращает всё, что она записала в лог.
pub(super) fn with_captured_log<R>(action: impl FnOnce() -> R) -> (R, String) {
    let captured = CapturedLog::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let result = tracing::subscriber::with_default(subscriber, action);
    (result, captured.text())
}
