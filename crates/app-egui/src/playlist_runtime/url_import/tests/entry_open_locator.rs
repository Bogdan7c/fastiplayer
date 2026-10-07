//! Строки импортированной коллекции открываются по собственной ссылке ролика.
//!
//! Путь настоящий: fake-запуск `yt-dlp` печатает JSON -> `extract_topology` -> S16 mapper ->
//! S08 preview/commit -> Row Play -> media-open intent. Подменён только дочерний процесс.

use std::io;
use std::process::{Child, Command, Stdio};

use service_ytdlp::{ExtractorProcessInvocation, ExtractorProcessLauncher, YtDlpExtractorAdapter};

use super::*;
use crate::playlist_runtime::controller::ControllerPlayItemOutcome;
use crate::playlist_runtime::{PlaylistMediaOpenGateError, RuntimeRowPlayOutcome};

const PLAYLIST_URL: &str = "https://www.youtube.com/playlist?list=PLroot";
const ENTRY_URL: &str = "https://www.youtube.com/watch?v=entry1";

/// Вместо настоящего `yt-dlp` печатает заранее заданные JSON-строки topology.
struct PrintTopologyLauncher {
    stdout_lines: Vec<String>,
}

impl ExtractorProcessLauncher for PrintTopologyLauncher {
    fn spawn(
        &self,
        _command: &mut Command,
        _invocation: ExtractorProcessInvocation,
    ) -> io::Result<Child> {
        use std::os::unix::process::CommandExt;

        let mut script = String::new();
        for line in &self.stdout_lines {
            script.push_str(&format!("printf '%s\\n' '{line}'\n"));
        }
        let mut fake = Command::new("sh");
        fake.arg("-c")
            .arg(script)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        fake.spawn()
    }
}

/// Плейлист из двух строк: ролик со ссылкой и ролик только с идентификатором экстрактора.
fn playlist_topology_lines() -> Vec<String> {
    let linked = format!(
        r#"{{"_type":"url","ie_key":"Youtube","id":"entry1","url":"{ENTRY_URL}","title":"One"}}"#
    );
    let id_only = r#"{"_type":"url","ie_key":"Youtube","id":"entry2","title":"Two"}"#.to_owned();
    let root = format!(
        r#"{{"_type":"playlist","id":"PLroot","title":"List","webpage_url":"{PLAYLIST_URL}","entries":[{linked},{id_only}]}}"#
    );
    vec![linked, id_only, root]
}

/// Add URL плейлиста через production resolver с fake-процессом; возвращает runtime с очередью.
fn runtime_with_imported_playlist() -> PlaylistRuntime {
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    runtime.resolve_missing_state_for_test();
    let launcher = Arc::new(PrintTopologyLauncher {
        stdout_lines: playlist_topology_lines(),
    });
    runtime
        .url_import
        .replace_resolver_for_test(Arc::new(ServicePlaylistUrlTopologyResolver {
            extractor_adapter: YtDlpExtractorAdapter::with_process_launcher(launcher),
        }));
    let config = YtDlpConfig {
        enabled: true,
        ..YtDlpConfig::default()
    };
    assert_eq!(
        runtime
            .append_playlist_url(PLAYLIST_URL, &config)
            .expect("topology job admission"),
        UrlAppendActionOutcome::ResolvingTopology
    );
    wait_until(|| {
        let _visible_change = runtime.drain_playlist_url_import_job();
        runtime.pending_playlist_import_preview().is_some()
    });
    let preview_id = runtime
        .pending_playlist_import_preview()
        .expect("preview")
        .preview_id();
    assert_eq!(
        runtime.continue_playlist_import(preview_id),
        PlaylistImportContinueOutcome::AwaitingConfirmation
    );
    let confirmation = runtime.pending_playlist_confirmation().expect("ack");
    assert!(matches!(
        runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
            intent_id: confirmation.intent_id(),
            decision: QueueReplacementConfirmationDecision::Confirm,
        }),
        PlaylistConfirmationApplyOutcome::Import(PlaylistImportContinueOutcome::Committed(_))
    ));
    runtime
}

fn row_ids(runtime: &PlaylistRuntime) -> Vec<playlist_core::PlaylistItemId> {
    runtime
        .controller
        .as_ref()
        .expect("controller")
        .queue()
        .iter_playable_ids()
        .collect()
}

fn url_of(locator: &playlist_core::PlaylistLocator) -> &str {
    locator
        .as_secret_url()
        .expect("URL locator")
        .expose_secret_for_open()
}

#[test]
fn row_play_of_imported_playlist_entry_opens_entry_watch_url() {
    let mut runtime = runtime_with_imported_playlist();
    let ids = row_ids(&runtime);
    assert_eq!(ids.len(), 2);
    // Persisted shape не менялся: identity строки — всё ещё ссылка на плейлист.
    let stored = runtime
        .controller
        .as_ref()
        .expect("controller")
        .queue()
        .item(ids[0])
        .expect("row");
    assert_eq!(url_of(stored.locator()), PLAYLIST_URL);

    let RuntimeRowPlayOutcome::Controller(ControllerPlayItemOutcome::StartInstall {
        install, ..
    }) = runtime.play_playlist_row(ids[0])
    else {
        panic!("idle row Play starts install");
    };
    let intent = runtime
        .media_open_intent_for_planned_install(&install)
        .expect("open intent");

    assert_eq!(url_of(intent.locator()), ENTRY_URL);
}

#[test]
fn row_play_of_entry_without_url_is_refused_instead_of_opening_the_playlist() {
    let mut runtime = runtime_with_imported_playlist();
    let ids = row_ids(&runtime);

    let RuntimeRowPlayOutcome::Controller(ControllerPlayItemOutcome::StartInstall {
        install, ..
    }) = runtime.play_playlist_row(ids[1])
    else {
        panic!("idle row Play starts install");
    };
    let refusal = runtime
        .media_open_intent_for_planned_install(&install)
        .err()
        .expect("id-only entry has no standalone open locator");

    assert!(matches!(
        refusal,
        PlaylistMediaOpenGateError::OperationalLocatorRefused(_)
    ));
}
