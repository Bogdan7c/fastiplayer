//! Учёт OS spawn attempts отдельно от реально созданных extractor-процессов.

use super::*;

#[derive(Debug, Clone, Copy)]
pub(super) enum SpawnAttemptOutcome {
    Started { pid: u32 },
    Failed { errno: Option<i32> },
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SpawnAttempt {
    pub(super) invocation: ExtractorProcessInvocation,
    pub(super) outcome: SpawnAttemptOutcome,
}

/// Не скрывает failures: разрешён только ограниченный production retry ETXTBSY
/// с неизменными reason/phase и последующим успешным созданием процесса.
pub(super) fn successful_invocations_from_attempts(
    attempts: &[SpawnAttempt],
) -> Vec<ExtractorProcessInvocation> {
    let mut successful = Vec::new();
    let mut pending_retry = None;
    for attempt in attempts {
        if let Some(invocation) = pending_retry {
            assert_eq!(
                attempt.invocation, invocation,
                "retry changed invocation identity"
            );
        }
        match attempt.outcome {
            SpawnAttemptOutcome::Started { pid } => {
                assert!(pid > 0, "successful spawn must expose a real PID");
                successful.push(attempt.invocation);
                pending_retry = None;
            }
            SpawnAttemptOutcome::Failed { errno } => {
                assert_eq!(errno, Some(libc::ETXTBSY), "unexpected spawn failure");
                pending_retry = Some(attempt.invocation);
            }
        }
    }
    assert!(
        pending_retry.is_none(),
        "spawn retry never created a process"
    );
    successful
}

/// Writer закрывается после настоящего отказа ОС, до следующей production attempt.
struct ReleaseWriterAfterBusy {
    spy: Arc<HermeticSpyLauncher>,
    writer: Mutex<Option<fs::File>>,
}

impl ExtractorProcessLauncher for ReleaseWriterAfterBusy {
    fn spawn(
        &self,
        command: &mut Command,
        invocation: ExtractorProcessInvocation,
    ) -> io::Result<Child> {
        let result = self.spy.spawn(command, invocation);
        if result
            .as_ref()
            .is_err_and(|error| error.raw_os_error() == Some(libc::ETXTBSY))
        {
            drop(self.writer.lock().expect("executable writer lock").take());
        }
        result
    }
}

#[test]
fn page_snapshot_retries_real_busy_executable_without_counting_a_phantom_process() {
    let fixture = HermeticFixtureDirectory::create("busy-page");
    fixture.install_yt_dlp(
        r#"#!/bin/sh
printf '%s\n' '{"title":"Busy page fixture","formats":[{"format_id":"http","url":"https://media.invalid/video.mp4","protocol":"https","ext":"mp4","vcodec":"avc1.42001E","acodec":"mp4a.40.2"}]}'
"#,
    );
    let writer = fs::OpenOptions::new()
        .write(true)
        .open(fixture.path().join("yt-dlp"))
        .expect("hold executable writer");
    let spy = Arc::new(HermeticSpyLauncher::new(fixture.path()));
    let launcher = Arc::new(ReleaseWriterAfterBusy {
        spy: Arc::clone(&spy),
        writer: Mutex::new(Some(writer)),
    });
    let adapter = YtDlpExtractorAdapter::with_process_launcher(launcher);
    let locator =
        crate::parse_yt_dlp_media_locator("https://example.invalid/page").expect("page locator");
    let snapshot = adapter
        .resolve_candidate_snapshot_with_cancellation(
            &locator,
            SourceIdentity::new(3019),
            ExtractionGeneration::new(1),
            &extractor_config(Duration::from_secs(2)),
            ExtractorInvocationReason::PageMediaResolution,
            &|| false,
        )
        .expect("real busy-file retry must reach parsed candidate snapshot");
    assert_eq!(snapshot.accepted_candidates().count(), 1);
    assert_eq!(
        snapshot.playlist_metadata().title(),
        Some("Busy page fixture")
    );
    let expected = ExtractorProcessInvocation::new(
        ExtractorInvocationReason::PageMediaResolution,
        ExtractorProcessPhase::CandidatePrimary,
    );
    let attempts = spy.attempts();
    assert_eq!(
        attempts.len(),
        2,
        "one busy attempt then one actual process"
    );
    assert!(matches!(
        attempts[0].outcome,
        SpawnAttemptOutcome::Failed {
            errno: Some(libc::ETXTBSY)
        }
    ));
    assert!(matches!(
        attempts.last().expect("spawn attempts").outcome,
        SpawnAttemptOutcome::Started { .. }
    ));
    assert!(
        attempts
            .iter()
            .all(|attempt| attempt.invocation == expected)
    );
    assert_eq!(spy.successful_invocations(), vec![expected]);
}
