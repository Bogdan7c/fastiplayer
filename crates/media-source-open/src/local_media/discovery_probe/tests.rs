//! Сквозные проверки probe метаданных плейлиста на реальных файлах.
//!
//! Главный тест гонит настоящий `DiscoveryExecutor` по папке с Dolby-соседями —
//! так же, как приложение после открытия файла — и сверяет длительность и тип
//! каждой записи с тем, что даёт playback open того же файла.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use fastiplayer_config::PlayerDemuxConfig;
use media_core::MediaDuration;
use playlist_discovery::{
    DiscoveryEvent, DiscoveryExecutor, DiscoveryFinalOutcome, DiscoveryFinalSummary,
    DiscoveryJobHandle, DiscoveryProbe, DiscoveryRecord, DiscoveryRequest,
    DiscoveryRequestRevision, DiscoveryWakePort, LocalMediaKind, ProbeOneLocalMediaError,
    SiblingDiscoveryPolicySnapshot, SiblingDiscoveryRequest, SiblingFilter, SiblingPolicyRevision,
    WakeDisconnected, build_directory_manifest,
};
use source_core::{CancellationToken, LocalFileSource};

use super::super::{AttachmentReadLimits, open_local_demuxer_from_source};
use super::LocalRegistryDiscoveryProbe;
use crate::ac3_test_support::{STEREO_AC3, SURROUND_EAC3};

/// ATSC AC-3 stereo + H.264 в MPEG-TS (README фикстуры — рядом с файлом).
const H264_AC3_STEREO_TS: &[u8] =
    include_bytes!("../../../../mpeg-ts-demux/tests/fixtures/ac3/h264-ac3-stereo-48k.ts");
/// Обе сырые Dolby-фикстуры — 16 кадров по 1536 сэмплов при 48 кГц.
const RAW_DOLBY_FIXTURE_DURATION: Duration = Duration::from_millis(512);
/// Сколько ждём terminal summary job-а discovery (запас на нагруженный CI).
const DISCOVERY_JOB_DEADLINE: Duration = Duration::from_secs(10);

/// Wake приложению здесь не нужен: тест сам опрашивает mailbox job-а.
struct PollingWake;

impl DiscoveryWakePort for PollingWake {
    fn wake(&self) -> Result<(), WakeDisconnected> {
        Ok(())
    }
}

fn write_fixture(directory: &Path, file_name: &str, bytes: &[u8]) -> PathBuf {
    let path = directory.join(file_name);
    std::fs::write(&path, bytes).expect("write fixture");
    path
}

/// Длительность, которую видит воспроизведение того же файла (эталон для плейлиста).
fn playback_open_duration(path: &Path) -> Option<Duration> {
    let source = LocalFileSource::open(path).expect("open local fixture");
    let extension = path.extension().and_then(|value| value.to_str());
    open_local_demuxer_from_source(
        source,
        extension,
        &PlayerDemuxConfig::default(),
        AttachmentReadLimits::default(),
        CancellationToken::never_cancelled(),
    )
    .expect("playback registry opens fixture")
    .duration()
}

/// Ждёт terminal summary и собирает все записи job-а.
///
/// Batch-и публикуются раньше terminal-а, поэтому после summary mailbox
/// опрашивается ещё раз (инвариант F1 из `playlist-discovery`).
fn collect_job_records(
    handle: &DiscoveryJobHandle,
) -> (DiscoveryFinalSummary, Vec<DiscoveryRecord>) {
    let deadline = Instant::now() + DISCOVERY_JOB_DEADLINE;
    let mut records = Vec::new();
    loop {
        drain_records_into(handle, &mut records);
        if let Some(summary) = handle.take_final_summary() {
            drain_records_into(handle, &mut records);
            return (summary, records);
        }
        assert!(Instant::now() < deadline, "discovery job did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn drain_records_into(handle: &DiscoveryJobHandle, records: &mut Vec<DiscoveryRecord>) {
    for event in handle.drain_events() {
        if let DiscoveryEvent::AdmittedBatch(batch) = event {
            records.extend(batch.records().iter().cloned());
            // ACK держит frontier sibling-а открытым так же, как это делает приложение.
            let _ack_outcome = handle.acknowledge_admitted_batch(batch.batch_id());
        }
    }
}

fn record_named<'records>(
    records: &'records [DiscoveryRecord],
    file_name: &str,
) -> &'records DiscoveryRecord {
    records
        .iter()
        .find(|record| record.media().display_filename() == file_name)
        .unwrap_or_else(|| panic!("no discovery record for {file_name}"))
}

/// Папка как у пользователя: открыт `.ac3`, рядом `.eac3`, `.ec3`, `.ts` и не-медиа.
/// Соседи получают ту же длительность и тот же тип, что увидит воспроизведение;
/// не-медиа и мусор с Dolby-расширением становятся «формат не поддерживается».
#[test]
fn sibling_discovery_of_dolby_folder_takes_metadata_from_playback_registry() {
    let directory = tempfile::tempdir().expect("temp directory");
    let target = write_fixture(directory.path(), "01-opened.ac3", STEREO_AC3);
    write_fixture(directory.path(), "02-surround.eac3", SURROUND_EAC3);
    write_fixture(directory.path(), "03-stereo.ec3", STEREO_AC3);
    let broadcast = write_fixture(directory.path(), "04-broadcast.ts", H264_AC3_STEREO_TS);
    write_fixture(directory.path(), "05-notes.txt", b"not a media file at all");
    write_fixture(directory.path(), "06-garbage.eac3", &[0x5a; 4096]);

    let executor = DiscoveryExecutor::start_with_probe(
        Arc::new(LocalRegistryDiscoveryProbe::new()),
        Arc::new(PollingWake),
    )
    .expect("discovery executor starts");
    let request = SiblingDiscoveryRequest::new(
        Arc::new(build_directory_manifest(&target).expect("manifest of fixture folder")),
        LocalMediaKind::AudioOnly,
        SiblingDiscoveryPolicySnapshot::new(
            true, // load_siblings
            SiblingFilter::AllMedia,
            SiblingPolicyRevision::new(1),
        ),
        DiscoveryRequestRevision::new(1),
    );
    let handle = executor
        .submit(DiscoveryRequest::Sibling(request))
        .expect("submit sibling discovery");
    let (summary, records) = collect_job_records(&handle);

    assert_eq!(summary.outcome, DiscoveryFinalOutcome::Completed);
    assert_eq!(summary.verified, 3, "eac3 + ec3 + ts are playable siblings");
    assert_eq!(records.len(), 3);
    assert_eq!(summary.failed, 2);
    assert_eq!(summary.failure_counts.unsupported_container, 2);
    assert_eq!(summary.failure_counts.probe_failed, 0);

    let surround = record_named(&records, "02-surround.eac3").media();
    assert_eq!(surround.media_kind(), LocalMediaKind::AudioOnly);
    assert_eq!(
        surround.duration(),
        Some(MediaDuration::from_duration(RAW_DOLBY_FIXTURE_DURATION))
    );
    let stereo = record_named(&records, "03-stereo.ec3").media();
    assert_eq!(stereo.media_kind(), LocalMediaKind::AudioOnly);
    assert_eq!(
        stereo.duration(),
        Some(MediaDuration::from_duration(RAW_DOLBY_FIXTURE_DURATION))
    );
    let broadcast_record = record_named(&records, "04-broadcast.ts").media();
    assert_eq!(
        broadcast_record.media_kind(),
        LocalMediaKind::VideoContaining
    );
    assert!(
        broadcast_record.duration().is_some(),
        "MPEG-TS sibling gets a duration"
    );
    assert_eq!(
        broadcast_record.duration(),
        playback_open_duration(&broadcast).map(MediaDuration::from_duration),
        "playlist shows the same duration as playback"
    );
    assert_eq!(
        surround.fingerprint().file_size_bytes(),
        SURROUND_EAC3.len() as u64
    );
}

/// Записи `log` уровня Error, пришедшие из текущего потока теста.
///
/// Logger процесса ставится один раз; фильтр по потоку отделяет записи этого
/// теста от параллельных тестов того же бинарника.
mod error_log_capture {
    use super::{OnceLock, RefCell};

    thread_local! {
        static CAPTURED_ERRORS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    struct ThreadErrorLogger;

    impl log::Log for ThreadErrorLogger {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata.level() <= log::Level::Error
        }

        fn log(&self, record: &log::Record<'_>) {
            if self.enabled(record.metadata()) {
                CAPTURED_ERRORS
                    .with(|captured| captured.borrow_mut().push(record.args().to_string()));
            }
        }

        fn flush(&self) {}
    }

    static LOGGER: ThreadErrorLogger = ThreadErrorLogger;
    static INSTALLED: OnceLock<()> = OnceLock::new();

    /// Выполняет `action` и возвращает Error-записи, сделанные этим потоком.
    pub(super) fn errors_logged_during(action: impl FnOnce()) -> Vec<String> {
        INSTALLED.get_or_init(|| {
            log::set_logger(&LOGGER).expect("no other logger in media-source-open tests");
            log::set_max_level(log::LevelFilter::Error);
        });
        CAPTURED_ERRORS.with(|captured| captured.borrow_mut().clear());
        action();
        CAPTURED_ERRORS.with(|captured| captured.take())
    }
}

/// Поддерживаемые Dolby/TS файлы не порождают ERROR-записей в логе, а прежний
/// Symphonia-only probe на тех же байтах порождал (контроль, что ловушка логов работает).
#[test]
fn probing_supported_dolby_and_ts_files_logs_no_errors() {
    let directory = tempfile::tempdir().expect("temp directory");
    let raw_ac3 = write_fixture(directory.path(), "song.ac3", STEREO_AC3);
    let raw_eac3 = write_fixture(directory.path(), "movie.eac3", SURROUND_EAC3);
    let broadcast = write_fixture(directory.path(), "show.ts", H264_AC3_STEREO_TS);
    let probe = LocalRegistryDiscoveryProbe::new();
    let cancellation = CancellationToken::new();

    let registry_errors = error_log_capture::errors_logged_during(|| {
        for path in [&raw_ac3, &raw_eac3, &broadcast] {
            probe
                .probe(path, &cancellation)
                .unwrap_or_else(|error| panic!("{} must be probed: {error}", path.display()));
        }
    });
    assert!(
        registry_errors.is_empty(),
        "registry probe logged errors: {registry_errors:?}"
    );

    let symphonia_only_errors = error_log_capture::errors_logged_during(|| {
        let _old_outcome = playlist_discovery::probe_one_local_media(&raw_ac3, &cancellation);
    });
    assert!(
        symphonia_only_errors
            .iter()
            .any(|message| message.contains("probe reached EOF")),
        "control: Symphonia-only probe should log the EOF error, got {symphonia_only_errors:?}"
    );
}

/// Файл не того типа — типизированный `UnsupportedContainer`, а не I/O и не «битый».
#[test]
fn unsupported_file_is_reported_as_unsupported_container() {
    let directory = tempfile::tempdir().expect("temp directory");
    let text_file = write_fixture(directory.path(), "readme.txt", b"plain text, no media");
    let empty_file = write_fixture(directory.path(), "empty.ac3", b"");
    let probe = LocalRegistryDiscoveryProbe::new();

    for path in [&text_file, &empty_file] {
        let error = probe
            .probe(path, &CancellationToken::new())
            .expect_err("non-media file must not produce a record");
        assert!(
            matches!(error, ProbeOneLocalMediaError::UnsupportedContainer { .. }),
            "{}: unexpected outcome {error:?}",
            path.display()
        );
    }
}

/// Отсутствующий файл сохраняет точный `io::ErrorKind`.
#[test]
fn missing_file_keeps_not_found_io_kind() {
    let directory = tempfile::tempdir().expect("temp directory");
    let error = LocalRegistryDiscoveryProbe::new()
        .probe(
            &directory.path().join("gone.eac3"),
            &CancellationToken::new(),
        )
        .expect_err("missing file cannot be probed");
    assert!(
        matches!(&error, ProbeOneLocalMediaError::IoFailure(io_error)
            if io_error.kind() == std::io::ErrorKind::NotFound),
        "unexpected outcome {error:?}"
    );
}

/// Отменённый probe отвечает `Cancelled` даже для несуществующего пути — до любого I/O.
#[test]
fn cancelled_probe_returns_cancelled_before_file_io() {
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = LocalRegistryDiscoveryProbe::new()
        .probe(Path::new("missing-but-cancelled.ac3"), &cancellation)
        .expect_err("cancelled probe returns no record");
    assert!(matches!(error, ProbeOneLocalMediaError::Cancelled));
}

/// Fingerprint probe-а совпадает с лёгкой проверкой кэша, поэтому кэш строки
/// плейлиста после probe-а признаётся актуальным без повторного demux.
#[test]
fn probe_fingerprint_matches_cache_validation_fingerprint() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = write_fixture(directory.path(), "dolby.eac3", SURROUND_EAC3);
    let probe = LocalRegistryDiscoveryProbe::new();
    let cancellation = CancellationToken::new();

    let record = probe.probe(&path, &cancellation).expect("probe fixture");
    let cache_fingerprint = probe
        .read_fingerprint(&path, &cancellation)
        .expect("read fingerprint");

    assert_eq!(record.fingerprint(), cache_fingerprint);
    assert_eq!(record.display_filename(), "dolby.eac3");
}
