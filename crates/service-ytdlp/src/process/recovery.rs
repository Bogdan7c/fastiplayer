use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use url::Url;

use super::{
    YtDlpProcessConfig, ensure_yt_dlp_candidate_success, run_dump_single_json,
    run_process_with_extractor_invocation,
};
use crate::embed_recovery::{
    GenericExtractorImpersonation, discover_non_platform_embed_urls, discover_page_title,
    should_attempt_platform_embed_recovery, write_pages_arguments,
};
use crate::error::YtDlpServiceError;
use crate::invocation::ExtractorProcessPhase;

pub(super) const MAX_RECOVERY_DUMP_FILES: usize = 8;
pub(super) const MAX_RECOVERY_DUMP_BYTES: u64 = 2 * 1024 * 1024;
const MAX_RECOVERY_TOTAL_BYTES: u64 = 8 * 1024 * 1024;
static RECOVERY_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Process-owned каталог с безусловной best-effort очисткой при выходе из scope.
struct RecoveryTempDirectory {
    path: PathBuf,
}

impl RecoveryTempDirectory {
    fn create() -> Result<Self, YtDlpServiceError> {
        let base = std::env::temp_dir();
        for _ in 0..16 {
            let sequence = RECOVERY_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = base.join(format!(
                "fastiplayer-ytdlp-recovery-{}-{timestamp}-{sequence}",
                std::process::id()
            ));
            let mut directory_builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                directory_builder.mode(0o700);
            }
            match directory_builder.create(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(YtDlpServiceError::process(error)),
            }
        }

        Err(YtDlpServiceError::process(anyhow::anyhow!(
            "не удалось выделить уникальный recovery-каталог"
        )))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RecoveryTempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Debug)]
pub(super) struct RecoveryDumpEvidence {
    pub(super) candidates: Vec<String>,
    page_title: Option<String>,
}

/// Общая candidate/topology граница восстановления после подтверждённого platform hijack.
pub(crate) fn recover_playable_document_after_platform_hijack(
    input_url: &str,
    primary_document: &Value,
    process_config: &YtDlpProcessConfig,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Option<Value>, YtDlpServiceError> {
    if !should_attempt_platform_embed_recovery(input_url, primary_document) {
        return Ok(None);
    }

    recover_non_platform_embed(input_url, process_config, is_cancelled)
}

fn recover_non_platform_embed(
    input_url: &str,
    process_config: &YtDlpProcessConfig,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Option<Value>, YtDlpServiceError> {
    if is_cancelled() {
        return Err(YtDlpServiceError::Cancellation);
    }

    let recovery_directory = RecoveryTempDirectory::create()?;
    let write_pages_arguments = write_pages_arguments(input_url);
    let write_pages_output = run_process_with_extractor_invocation(
        process_config.executable.as_str(),
        &write_pages_arguments,
        Some(recovery_directory.path()),
        process_config.timeout,
        process_config.output_budgets(),
        process_config.launch_context(ExtractorProcessPhase::RecoveryPageCapture),
        is_cancelled,
    )?;
    ensure_yt_dlp_candidate_success(write_pages_output.status, write_pages_output.stderr_bytes)?;

    let evidence =
        read_recovery_embed_candidates(recovery_directory.path(), input_url, is_cancelled)?;
    for embed_url in &evidence.candidates {
        let mut recovered_document = match run_dump_single_json(
            embed_url,
            GenericExtractorImpersonation::RequiredForHttp,
            process_config,
            ExtractorProcessPhase::RecoveryEmbedCandidate,
            is_cancelled,
        ) {
            Ok(document) => document,
            Err(YtDlpServiceError::Cancellation) => {
                return Err(YtDlpServiceError::Cancellation);
            }
            Err(_) => continue,
        };
        if should_attempt_platform_embed_recovery(input_url, &recovered_document) {
            continue;
        }
        enrich_recovered_document_title(&mut recovered_document, evidence.page_title.as_deref());
        return Ok(Some(recovered_document));
    }

    Ok(None)
}

pub(super) fn read_recovery_embed_candidates(
    directory: &Path,
    input_url: &str,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<RecoveryDumpEvidence, YtDlpServiceError> {
    let mut dump_paths = Vec::new();
    for entry in fs::read_dir(directory).map_err(YtDlpServiceError::process)? {
        if is_cancelled() {
            return Err(YtDlpServiceError::Cancellation);
        }
        let entry = entry.map_err(YtDlpServiceError::process)?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "dump")
        {
            dump_paths.push(path);
            if dump_paths.len() > MAX_RECOVERY_DUMP_FILES {
                return Ok(RecoveryDumpEvidence {
                    candidates: Vec::new(),
                    page_title: None,
                });
            }
        }
    }
    dump_paths.sort();

    let mut total_bytes = 0_u64;
    let mut dumped_html = String::new();
    let input_host = Url::parse(input_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase));
    let mut matching_page_title = None;
    let mut fallback_page_title = None;
    for path in dump_paths {
        if is_cancelled() {
            return Err(YtDlpServiceError::Cancellation);
        }
        let metadata = fs::metadata(&path).map_err(YtDlpServiceError::process)?;
        if !metadata.is_file() || metadata.len() > MAX_RECOVERY_DUMP_BYTES {
            return Ok(RecoveryDumpEvidence {
                candidates: Vec::new(),
                page_title: None,
            });
        }
        total_bytes = total_bytes.saturating_add(metadata.len());
        if total_bytes > MAX_RECOVERY_TOTAL_BYTES {
            return Ok(RecoveryDumpEvidence {
                candidates: Vec::new(),
                page_title: None,
            });
        }

        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        fs::File::open(&path)
            .map_err(YtDlpServiceError::process)?
            .take(MAX_RECOVERY_DUMP_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(YtDlpServiceError::process)?;
        if bytes.len() as u64 > MAX_RECOVERY_DUMP_BYTES {
            return Ok(RecoveryDumpEvidence {
                candidates: Vec::new(),
                page_title: None,
            });
        }
        let html = String::from_utf8_lossy(&bytes);
        if let Some(title) = discover_page_title(&html) {
            fallback_page_title.get_or_insert_with(|| title.clone());
            if matching_page_title.is_none()
                && input_host
                    .as_deref()
                    .is_some_and(|host| html.to_ascii_lowercase().contains(host))
            {
                matching_page_title = Some(title);
            }
        }
        dumped_html.push_str(&html);
        dumped_html.push('\n');
    }
    if is_cancelled() {
        return Err(YtDlpServiceError::Cancellation);
    }
    let candidates = discover_non_platform_embed_urls(&dumped_html);
    if is_cancelled() {
        return Err(YtDlpServiceError::Cancellation);
    }

    Ok(RecoveryDumpEvidence {
        candidates,
        page_title: matching_page_title.or(fallback_page_title),
    })
}

pub(super) fn enrich_recovered_document_title(document: &mut Value, page_title: Option<&str>) {
    let Some(page_title) = page_title.filter(|title| !title.trim().is_empty()) else {
        return;
    };
    let Some(object) = document.as_object_mut() else {
        return;
    };
    let needs_page_title = object
        .get("title")
        .and_then(Value::as_str)
        .is_none_or(|title| title.trim().is_empty() || title.trim().eq_ignore_ascii_case("video"));
    if needs_page_title {
        object.insert("title".to_owned(), Value::String(page_title.to_owned()));
    }
}
