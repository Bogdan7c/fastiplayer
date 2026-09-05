use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use fastiplayer_config::YtDlpConfig;
use web_media_core::{ExtractionGeneration, ExtractorInvocationReason, SourceIdentity};

use super::{
    ExtractorProcessInvocation, ExtractorProcessLauncher, ExtractorProcessPhase,
    YtDlpExtractorAdapter,
};

/// Уникальный suffix исключает пересечение parallel hermetic fixtures.
static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Владелец одного hermetic `yt-dlp` executable и его marker files.
struct HermeticFixtureDirectory {
    /// Absolute directory, добавляемая только в environment конкретного Command.
    path: PathBuf,
}

impl HermeticFixtureDirectory {
    /// Создаёт уникальную fixture directory без изменения process-wide PATH.
    fn create(label: &str) -> Self {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fastiplayer-n03-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create N03 hermetic fixture directory");
        Self { path }
    }

    /// Возвращает directory для launcher environment и marker assertions.
    fn path(&self) -> &Path {
        &self.path
    }

    /// Устанавливает executable script под production именем `yt-dlp`.
    fn install_yt_dlp(&self, script: &str) {
        let executable = self.path.join("yt-dlp");
        fs::write(&executable, script).expect("write hermetic yt-dlp script");
        let mut permissions = fs::metadata(&executable)
            .expect("read hermetic yt-dlp metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(executable, permissions).expect("make hermetic yt-dlp executable");
    }
}

impl Drop for HermeticFixtureDirectory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            eprintln!("не удалось удалить N03 hermetic fixture: {error}");
        }
    }
}

/// Instance-owned launcher изолирует PATH и сохраняет outcome каждой spawn attempt.
struct HermeticSpyLauncher {
    /// Fixture directory с executable под production именем.
    executable_directory: PathBuf,
    /// Typed invocation events без argv/URL/output payload.
    attempts: Mutex<Vec<SpawnAttempt>>,
}

impl HermeticSpyLauncher {
    /// Создаёт launcher для одной fixture directory.
    fn new(executable_directory: &Path) -> Self {
        Self {
            executable_directory: executable_directory.to_path_buf(),
            attempts: Mutex::new(Vec::new()),
        }
    }

    /// Возвращает успешные запуски; неуспешные попытки остаются в полном журнале.
    /// Только предусмотренный production-контрактом ETXTBSY может предшествовать
    /// повтору той же invocation; другая ошибка либо смена reason/phase провалит тест.
    fn successful_invocations(&self) -> Vec<ExtractorProcessInvocation> {
        successful_invocations_from_attempts(&self.attempts())
    }

    fn attempts(&self) -> Vec<SpawnAttempt> {
        self.attempts
            .lock()
            .expect("hermetic spy invocation lock")
            .clone()
    }
}

impl ExtractorProcessLauncher for HermeticSpyLauncher {
    fn spawn(
        &self,
        command: &mut Command,
        invocation: ExtractorProcessInvocation,
    ) -> io::Result<Child> {
        let mut command_path = OsString::from(&self.executable_directory);
        if let Some(system_path) = std::env::var_os("PATH") {
            command_path.push(":");
            command_path.push(system_path);
        }
        command.env("PATH", command_path);
        let mut attempts = self
            .attempts
            .lock()
            .map_err(|_| io::Error::other("hermetic spy invocation lock poisoned"))?;
        let spawned = command.spawn();
        attempts.push(SpawnAttempt {
            invocation,
            outcome: match &spawned {
                Ok(child) => SpawnAttemptOutcome::Started { pid: child.id() },
                Err(error) => SpawnAttemptOutcome::Failed {
                    errno: error.raw_os_error(),
                },
            },
        });
        spawned
    }
}

/// Создаёт enabled bounded config; executable остаётся production `yt-dlp`.
fn extractor_config(timeout: Duration) -> YtDlpConfig {
    YtDlpConfig {
        resolve_timeout_ms: timeout.as_millis() as u64,
        ..YtDlpConfig::default()
    }
}

/// Public adapter сохраняет YouTube-like formats/metadata и exact reason.
#[cfg(unix)]
#[test]
fn youtube_like_snapshot_preserves_formats_metadata_and_page_reason() {
    let fixture = HermeticFixtureDirectory::create("youtube");
    fixture.install_yt_dlp(
        r#"#!/bin/sh
printf '%s\n' '{"title":"YouTube-like title","duration":42,"is_live":false,"live_status":"not_live","webpage_url":"https://www.youtube.com/watch?v=fixture","extractor_key":"Youtube","formats":[{"format_id":"18","url":"https://media.invalid/18.mp4","protocol":"https","ext":"mp4","container":"mp4","vcodec":"avc1.42001E","acodec":"mp4a.40.2","dynamic_range":"SDR"},{"format_id":"251","url":"https://media.invalid/251.webm","protocol":"https","ext":"webm","container":"webm","vcodec":"none","acodec":"opus"}]}'
"#,
    );
    let spy = Arc::new(HermeticSpyLauncher::new(fixture.path()));
    let adapter = YtDlpExtractorAdapter::with_process_launcher(spy.clone());
    let locator = crate::parse_yt_dlp_media_locator("https://www.youtube.com/watch?v=fixture")
        .expect("parse YouTube-like fixture locator");

    let snapshot = adapter
        .resolve_candidate_snapshot_with_cancellation(
            &locator,
            SourceIdentity::new(3003),
            ExtractionGeneration::new(1),
            &extractor_config(Duration::from_secs(2)),
            ExtractorInvocationReason::PageMediaResolution,
            &|| false,
        )
        .expect("resolve YouTube-like candidate snapshot");

    assert_eq!(snapshot.accepted_candidates().count(), 2);
    assert_eq!(
        snapshot.playlist_metadata().title(),
        Some("YouTube-like title")
    );
    assert_eq!(
        snapshot.playlist_metadata().duration(),
        Some(Duration::from_secs(42))
    );
    assert_eq!(
        spy.successful_invocations(),
        vec![ExtractorProcessInvocation::new(
            ExtractorInvocationReason::PageMediaResolution,
            ExtractorProcessPhase::CandidatePrimary,
        )]
    );
}

/// Вторая public-page fixture получает тот же точный reason и ровно один spawn.
#[cfg(unix)]
#[test]
fn html_page_snapshot_uses_page_reason_and_one_primary_spawn() {
    let fixture = HermeticFixtureDirectory::create("html-page");
    fixture.install_yt_dlp(
        r#"#!/bin/sh
printf '%s\n' '{"title":"HTML page title","duration":12,"is_live":false,"live_status":"not_live","webpage_url":"https://www.w3schools.com/html/mov_bbb.mp4","extractor_key":"Generic","formats":[{"format_id":"http-720","url":"https://media.invalid/mov_bbb.mp4?token=ephemeral","protocol":"https","ext":"mp4","container":"mp4","vcodec":"avc1.42001E","acodec":"mp4a.40.2","http_headers":{"Authorization":"Bearer fixture-secret"},"cookies":"session=fixture-secret"}]}'
"#,
    );
    let spy = Arc::new(HermeticSpyLauncher::new(fixture.path()));
    let adapter = YtDlpExtractorAdapter::with_process_launcher(spy.clone());
    let locator =
        crate::parse_yt_dlp_media_locator("https://www.w3schools.com/html/html5_video.asp")
            .expect("parse HTML-page fixture locator");

    let snapshot = adapter
        .resolve_candidate_snapshot_with_cancellation(
            &locator,
            SourceIdentity::new(3004),
            ExtractionGeneration::new(1),
            &extractor_config(Duration::from_secs(2)),
            ExtractorInvocationReason::PageMediaResolution,
            &|| false,
        )
        .expect("resolve HTML-page candidate snapshot");

    assert_eq!(snapshot.accepted_candidates().count(), 1);
    assert_eq!(
        spy.successful_invocations(),
        vec![ExtractorProcessInvocation::new(
            ExtractorInvocationReason::PageMediaResolution,
            ExtractorProcessPhase::CandidatePrimary,
        )]
    );
    let safe_debug = format!("{snapshot:?}");
    for secret in ["ephemeral", "fixture-secret", "Authorization", "session="] {
        assert!(!safe_debug.contains(secret));
    }
}

/// Собирает production Rust sources, исключая отдельные test modules.
fn collect_production_rust_sources(directory: &Path, sources: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("service source directory должна читаться")
    {
        let path = entry.expect("service source entry должна читаться").path();
        if path.is_dir() {
            collect_production_rust_sources(&path, sources);
        } else if path.extension().is_some_and(|extension| extension == "rs")
            && path.file_name().is_none_or(|name| name != "tests.rs")
        {
            sources.push(path);
        }
    }
}

/// Все OS child starts обязаны пройти единственный instance-injected launcher.
#[test]
fn production_process_spawn_entrypoints_match_exact_injected_owner_allowlist() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut rust_sources = Vec::new();
    collect_production_rust_sources(&source_root, &mut rust_sources);
    let mut direct_spawn_sources = Vec::new();
    let mut owned_launcher_sources = Vec::new();

    for source_path in rust_sources {
        let relative_path = source_path
            .strip_prefix(&source_root)
            .expect("collected service source обязан быть внутри src")
            .to_string_lossy()
            .replace('\\', "/");
        let source = fs::read_to_string(&source_path).expect("service Rust source должен читаться");
        let direct_spawn_count = source.matches(".spawn()").count();
        direct_spawn_sources.extend(std::iter::repeat_n(
            relative_path.clone(),
            direct_spawn_count,
        ));
        let owned_launcher_count = source.matches("spawn_owned_process_with_launcher(").count();
        owned_launcher_sources.extend(std::iter::repeat_n(relative_path, owned_launcher_count));
    }
    direct_spawn_sources.sort();
    owned_launcher_sources.sort();

    assert_eq!(direct_spawn_sources, ["invocation.rs"]);
    assert_eq!(
        owned_launcher_sources,
        ["process.rs", "process_tree.rs", "topology/process.rs"]
    );
}

/// HTML platform-hijack recovery сохраняет formats/title и reason каждого subprocess-а.
#[cfg(unix)]
#[test]
fn html_page_recovery_spy_sees_every_phase_with_original_reason() {
    let fixture = HermeticFixtureDirectory::create("html-recovery");
    fixture.install_yt_dlp(
        r#"#!/bin/sh
last_argument=
for argument do
    if [ "$argument" = "--write-pages" ]; then
        printf '%s' '<link rel="canonical" href="https://catalog.example/watch/7"><title>HTML fixture title</title><iframe src="https://player.example/embed/7"></iframe>' > page.dump
        exit 0
    fi
    last_argument="$argument"
done
if [ "$last_argument" = "https://catalog.example/watch/7" ]; then
    printf '%s\n' '{"extractor_key":"Youtube","webpage_url":"https://www.youtube.com/watch?v=hijack"}'
else
    test "$last_argument" = "https://player.example/embed/7" || exit 91
    printf '%s\n' '{"title":"video","duration":9,"is_live":false,"formats":[{"format_id":"html-18","url":"https://media.invalid/html-18.mp4","protocol":"https","ext":"mp4","container":"mp4","vcodec":"avc1.42001E","acodec":"mp4a.40.2","dynamic_range":"SDR"}]}'
fi
"#,
    );
    let spy = Arc::new(HermeticSpyLauncher::new(fixture.path()));
    let adapter = YtDlpExtractorAdapter::with_process_launcher(spy.clone());
    let locator = crate::parse_yt_dlp_media_locator("https://catalog.example/watch/7")
        .expect("parse HTML fixture locator");

    let snapshot = adapter
        .resolve_candidate_snapshot_with_cancellation(
            &locator,
            SourceIdentity::new(3004),
            ExtractionGeneration::new(1),
            &extractor_config(Duration::from_secs(2)),
            ExtractorInvocationReason::PageMediaResolution,
            &|| false,
        )
        .expect("recover HTML-page candidate snapshot");

    assert_eq!(snapshot.accepted_candidates().count(), 1);
    assert_eq!(
        snapshot.playlist_metadata().title(),
        Some("HTML fixture title")
    );
    assert_eq!(
        snapshot.playlist_metadata().duration(),
        Some(Duration::from_secs(9))
    );
    assert_eq!(
        spy.successful_invocations()
            .into_iter()
            .map(|invocation| (invocation.reason(), invocation.phase()))
            .collect::<Vec<_>>(),
        vec![
            (
                ExtractorInvocationReason::PageMediaResolution,
                ExtractorProcessPhase::CandidatePrimary,
            ),
            (
                ExtractorInvocationReason::PageMediaResolution,
                ExtractorProcessPhase::RecoveryPageCapture,
            ),
            (
                ExtractorInvocationReason::PageMediaResolution,
                ExtractorProcessPhase::RecoveryEmbedCandidate,
            ),
        ]
    );
}

/// Topology entrypoint публикует collection reason через тот же launcher.
#[cfg(unix)]
#[test]
fn topology_uses_collection_reason_on_shared_launcher() {
    let fixture = HermeticFixtureDirectory::create("topology");
    fixture.install_yt_dlp(
        r#"#!/bin/sh
printf '%s\n' \
  '{"_type":"url","url":"https://delegate.invalid/1"}' \
  '{"_type":"playlist","id":"root","title":"Root","entries":[{"_type":"url","url":"https://delegate.invalid/1"}]}'
"#,
    );
    let spy = Arc::new(HermeticSpyLauncher::new(fixture.path()));
    let adapter = YtDlpExtractorAdapter::with_process_launcher(spy.clone());
    let locator = crate::parse_yt_dlp_media_locator("https://catalog.example/collection")
        .expect("parse topology fixture locator");

    let topology = adapter
        .extract_topology_with_budgets(
            &locator,
            &extractor_config(Duration::from_secs(2)),
            crate::YtDlpTopologyBudgets::default(),
            ExtractorInvocationReason::CollectionTopologyResolution,
            &|| false,
        )
        .expect("extract hermetic collection topology");

    assert!(topology.as_playlist().is_some());
    assert_eq!(
        spy.successful_invocations(),
        vec![ExtractorProcessInvocation::new(
            ExtractorInvocationReason::CollectionTopologyResolution,
            ExtractorProcessPhase::TopologyPrimary,
        )]
    );
}

/// Cancellation recovery завершает всю process group и сохраняет typed reason.
#[cfg(unix)]
#[test]
fn recovery_cancellation_reaps_descendant_through_injected_launcher() {
    let fixture = HermeticFixtureDirectory::create("cancel-cleanup");
    let descendant_record = fixture.path().join("descendant-pid");
    fixture.install_yt_dlp(&format!(
        r#"#!/bin/sh
for argument do
    if [ "$argument" = "--write-pages" ]; then
        sleep 30 &
        descendant=$!
        printf '%s' "$descendant" > "{}"
        wait
    fi
done
printf '%s\n' '{{"extractor_key":"Youtube","webpage_url":"https://www.youtube.com/watch?v=hijack"}}'
"#,
        descendant_record.display()
    ));
    let spy = Arc::new(HermeticSpyLauncher::new(fixture.path()));
    let adapter = YtDlpExtractorAdapter::with_process_launcher(spy.clone());
    let locator = crate::parse_yt_dlp_media_locator("https://catalog.example/watch/cancel")
        .expect("parse cancellation fixture locator");
    let started_at = Instant::now();

    let error = adapter
        .resolve_candidate_snapshot_with_cancellation(
            &locator,
            SourceIdentity::new(3005),
            ExtractionGeneration::new(1),
            &extractor_config(Duration::from_secs(10)),
            ExtractorInvocationReason::ExtractorBackedRecovery,
            &|| descendant_record.exists(),
        )
        .expect_err("recovery must preserve typed cancellation");

    assert!(matches!(error, crate::YtDlpServiceError::Cancellation));
    assert!(started_at.elapsed() < Duration::from_secs(2));
    let descendant_id: i32 = fs::read_to_string(&descendant_record)
        .expect("recovery records descendant pid")
        .parse()
        .expect("descendant pid is numeric");
    let cleanup_deadline = Instant::now() + Duration::from_secs(1);
    while process_is_running(descendant_id) && Instant::now() < cleanup_deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!process_is_running(descendant_id));
    assert_eq!(
        spy.successful_invocations()
            .into_iter()
            .map(ExtractorProcessInvocation::reason)
            .collect::<Vec<_>>(),
        vec![
            ExtractorInvocationReason::ExtractorBackedRecovery,
            ExtractorInvocationReason::ExtractorBackedRecovery,
        ]
    );
}

/// Проверяет существование PID без ownership над процессом.
#[cfg(unix)]
fn process_is_running(process_id: i32) -> bool {
    let result = unsafe { libc::kill(process_id, 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[path = "tests/spawn_attempts.rs"]
mod spawn_attempts;
use spawn_attempts::{SpawnAttempt, SpawnAttemptOutcome, successful_invocations_from_attempts};
