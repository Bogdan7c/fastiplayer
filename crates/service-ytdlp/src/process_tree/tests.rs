use std::io::Read;
use std::process::Stdio;

use web_media_core::ExtractorInvocationReason;

use super::*;
use crate::invocation::{ExtractorProcessPhase, YtDlpExtractorAdapter};

/// Запускает test child через тот же injected launcher boundary, что production.
fn spawn_owned_process(
    command: &mut Command,
    operation_started_at: Instant,
    timeout: Duration,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<OwnedProcess, OwnedProcessSpawnError> {
    let adapter = YtDlpExtractorAdapter::default();
    let launcher = adapter.process_launcher();
    spawn_owned_process_with_launcher(
        command,
        operation_started_at,
        timeout,
        is_cancelled,
        launcher.as_ref(),
        ExtractorProcessInvocation::new(
            ExtractorInvocationReason::PageMediaResolution,
            ExtractorProcessPhase::CandidatePrimary,
        ),
    )
}

/// Unix poll оставляет root waitable до group cleanup и сохраняет настоящий status.
#[test]
fn unix_root_exit_poll_does_not_reap_before_group_cleanup() {
    let mut command = Command::new("sh");
    command
        .args(["-c", "exit 7"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut process = spawn_owned_process(
        &mut command,
        Instant::now(),
        Duration::from_secs(2),
        &|| false,
    )
    .expect("spawn WNOWAIT fixture");
    let root_process_id = process.root_process_id();
    let observation_started_at = Instant::now();
    loop {
        if process.poll_root_exit().expect("WNOWAIT poll must succeed")
            == OwnedProcessRootState::Exited
        {
            break;
        }
        assert!(
            observation_started_at.elapsed() < Duration::from_secs(1),
            "root fixture must exit promptly"
        );
        thread::sleep(Duration::from_millis(1));
    }

    // Повторный raw WNOWAIT доказывает, что OwnedProcess ещё не reap-нул root.
    // SAFETY: zeroed siginfo_t является output buffer для waitid.
    let mut process_info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    // SAFETY: root PID всё ещё принадлежит этому process и остаётся waitable.
    let wait_result = unsafe {
        libc::waitid(
            libc::P_PID,
            libc::id_t::try_from(root_process_id).expect("test PID fits id_t"),
            &mut process_info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    assert_eq!(wait_result, 0);
    // SAFETY: successful waitid инициализировал SIGCHLD payload.
    assert_eq!(
        unsafe { process_info.si_pid() },
        root_process_id as libc::pid_t
    );

    let exit_status = process
        .finish()
        .expect("group cleanup must reap root exactly once");
    assert_eq!(exit_status.code(), Some(7));
}

/// Abort получает completion только после выхода worker и закрытия reader FD.
#[test]
fn pipe_reader_abort_confirms_worker_and_file_descriptor_teardown() {
    let mut command = Command::new("sh");
    command
        .args(["-c", "sleep 30"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut process = spawn_owned_process(
        &mut command,
        Instant::now(),
        Duration::from_secs(2),
        &|| false,
    )
    .expect("spawn pipe-abort fixture");
    let stdout = process.take_stdout().expect("stdout pipe configured");
    let reader = spawn_owned_pipe_reader("pipe-abort-test", stdout, |pipe| {
        let mut captured_bytes = Vec::new();
        pipe.read_to_end(&mut captured_bytes)?;
        Ok(captured_bytes)
    })
    .expect("spawn stop-aware reader");

    let abort_started_at = Instant::now();
    reader
        .abort()
        .expect("owner stop sentinel must complete worker without retry loop");
    assert!(
        abort_started_at.elapsed() < Duration::from_secs(1),
        "reader abort must not consume stop timeout"
    );
    process.finish().expect("cleanup pipe-abort fixture");
}

/// Setup abort через Drop завершает root+descendant и закрывает унаследованный pipe.
#[test]
fn setup_abort_drop_kills_descendant_and_unblocks_pipe_reader() {
    let mut command = Command::new("sh");
    command
        .args(["-c", "sleep 30 & printf ready; wait"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut process = spawn_owned_process(
        &mut command,
        Instant::now(),
        Duration::from_secs(2),
        &|| false,
    )
    .expect("spawn owned setup-abort fixture");
    let mut stdout = process.take_stdout().expect("stdout pipe configured");
    let mut ready = [0_u8; 5];
    stdout
        .read_exact(&mut ready)
        .expect("fixture confirms descendant creation");
    assert_eq!(&ready, b"ready");
    assert!(
        process.take_stderr().is_none(),
        "missing stderr simulates setup abort"
    );
    let pipe_reader = thread::spawn(move || {
        let mut remaining_stdout = Vec::new();
        stdout.read_to_end(&mut remaining_stdout)?;
        Ok::<_, io::Error>(remaining_stdout)
    });

    let cleanup_started_at = Instant::now();
    drop(process);
    let remaining_stdout = pipe_reader
        .join()
        .expect("pipe reader thread must not panic")
        .expect("pipe reader reaches EOF after owner drop");

    assert!(remaining_stdout.is_empty());
    assert!(
        cleanup_started_at.elapsed() < Duration::from_secs(2),
        "owner drop must not wait for the descendant sleep"
    );
}
