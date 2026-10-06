//! Причина отказа topology-процесса (UX сессия 09): распознанная метка `ERROR:` и
//! отсутствующая программа становятся типизированными, текст stderr не сохраняется.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::*;
use crate::rejection_reason::YtDlpRejectionReason;

/// Уникальный суффикс имён временных скриптов внутри процесса тестов.
static SCRIPT_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Создаёт исполняемый shell-скрипт во временном каталоге.
fn create_script(body: &str) -> PathBuf {
    let sequence = SCRIPT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "fastiplayer-ytdlp-topology-reason-{}-{sequence}.sh",
        std::process::id()
    ));
    fs::write(&path, body).expect("test script должен записаться");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
        .expect("test script должен стать executable");
    path
}

/// Запускает topology-процесс с production argv и короткими тестовыми лимитами.
fn run(executable: &std::path::Path) -> Result<TopologyProcessOutput, YtDlpTopologyError> {
    run_topology_process(
        executable.to_str().expect("UTF-8 test path"),
        "https://input.invalid/root",
        GenericExtractorImpersonation::RequiredForHttp,
        Duration::from_secs(2),
        YtDlpTopologyBudgets::default(),
        &|| false,
    )
}

#[test]
fn recognized_error_line_becomes_reason_without_keeping_stderr_text() {
    let script = create_script(
        "#!/bin/sh\nprintf 'WARNING: [youtube:tab] some entries are unavailable\\n' >&2\nprintf 'ERROR: [youtube] abc: Private video. Sign in https://x.invalid/?token=very-secret\\n' >&2\nexit 1\n",
    );

    let output = run(&script).expect("process layer возвращает status вызывающему");
    let _removed = fs::remove_file(&script);

    assert!(!output.status.success());
    assert_eq!(
        output.stderr.rejection_reason,
        YtDlpRejectionReason::PrivateMedia
    );
    assert!(!format!("{output:?}").contains("very-secret"));
}

#[test]
fn missing_executable_is_typed_not_found_instead_of_process_failure() {
    let missing_executable = std::env::temp_dir().join(format!(
        "fastiplayer-ytdlp-topology-missing-{}",
        std::process::id()
    ));

    let error = run(&missing_executable).expect_err("несуществующая программа не запускается");

    assert!(
        matches!(error, YtDlpTopologyError::ExecutableNotFound),
        "ожидался ExecutableNotFound, получено: {error:?}"
    );
}
