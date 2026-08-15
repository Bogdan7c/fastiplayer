//! Service-owned bounded extraction для `video/playlist/multi_video/url` topology.

mod limits;
mod model;
mod parser;
#[cfg(test)]
#[path = "parser/tests.rs"]
mod parser_tests;
mod process;
mod reopen;

use rustiplayer_config::YtDlpConfig;
use serde_json::{Value, json};

pub use limits::{
    DEFAULT_TOPOLOGY_DEPTH, DEFAULT_TOPOLOGY_ENTRY_COUNT, DEFAULT_TOPOLOGY_JSON_DEPTH,
    DEFAULT_TOPOLOGY_JSON_LINE_BYTES, DEFAULT_TOPOLOGY_STDERR_BYTES, DEFAULT_TOPOLOGY_STDOUT_BYTES,
    TOPOLOGY_IDENTITY_MAX_UTF8_BYTES, TOPOLOGY_LOCATOR_MAX_UTF8_BYTES,
    TOPOLOGY_SUMMARY_TEXT_MAX_UTF8_BYTES, YtDlpTopologyBudgetField, YtDlpTopologyBudgets,
    YtDlpTopologyError, YtDlpTopologyInvalidResponseReason,
};
pub use model::{
    YtDlpDelegationSummaryPolicy, YtDlpTopology, YtDlpTopologyCollection, YtDlpTopologyDelegation,
    YtDlpTopologyEntry, YtDlpTopologyEntryKind, YtDlpTopologyIdentity, YtDlpTopologyKind,
    YtDlpTopologyMultiVideo, YtDlpTopologySummary, YtDlpTopologySummaryFieldState,
    YtDlpTopologySummaryUnavailableReason, YtDlpTopologyVideo, YtDlpUnavailableTopologyEntry,
    YtDlpUnavailableTopologyReason,
};
pub use reopen::{
    YT_DLP_DURABLE_REOPEN_PAYLOAD_MAX_BYTES, YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION,
    YT_DLP_DURABLE_REOPEN_SERVICE_OWNER, YtDlpDurableReopenClassificationError,
    YtDlpDurableReopenIdentityInput, YtDlpDurableReopenMaterialKind, YtDlpDurableReopenPayload,
    classify_yt_dlp_delegation_reopen_target, classify_yt_dlp_durable_reopen_identity,
};

use crate::YtDlpMediaLocator;
use crate::error::YtDlpServiceError;
use crate::process::{YtDlpProcessConfig, recover_playable_document_after_platform_hijack};
use parser::{parse_topology_root, validate_lazy_json_lines};
use process::{TopologyProcessOutput, run_topology_process};

/// Извлекает bounded topology с default budgets.
pub fn extract_yt_dlp_topology_with_config(
    locator: &YtDlpMediaLocator,
    yt_dlp_config: &YtDlpConfig,
    is_cancelled: impl Fn() -> bool,
) -> Result<YtDlpTopology, YtDlpTopologyError> {
    extract_yt_dlp_topology_with_budgets(
        locator,
        yt_dlp_config,
        YtDlpTopologyBudgets::default(),
        is_cancelled,
    )
}

/// Извлекает bounded topology с explicit caller budgets.
pub fn extract_yt_dlp_topology_with_budgets(
    locator: &YtDlpMediaLocator,
    yt_dlp_config: &YtDlpConfig,
    budgets: YtDlpTopologyBudgets,
    is_cancelled: impl Fn() -> bool,
) -> Result<YtDlpTopology, YtDlpTopologyError> {
    if !yt_dlp_config.enabled {
        return Err(YtDlpTopologyError::AdapterDisabled);
    }
    let budgets = budgets.validate()?;
    let process_config = YtDlpProcessConfig::from_yt_dlp_config_for_topology(yt_dlp_config)?;
    let process_output = run_topology_process(
        process_config.executable_for_spawn(),
        locator.expose_secret_for_open(),
        crate::embed_recovery::GenericExtractorImpersonation::for_input_scheme(
            locator.input_scheme(),
        ),
        process_config.extraction_timeout(),
        budgets,
        &is_cancelled,
    )?;
    let primary_topology = topology_from_process_output(process_output, budgets)?;
    recover_topology_after_platform_hijack(primary_topology, budgets, |primary_document| {
        recover_playable_document_after_platform_hijack(
            locator.expose_secret_for_open(),
            primary_document,
            &process_config,
            &is_cancelled,
        )
    })
}

fn recover_topology_after_platform_hijack(
    primary_topology: YtDlpTopology,
    budgets: YtDlpTopologyBudgets,
    recover: impl FnOnce(&Value) -> Result<Option<Value>, YtDlpServiceError>,
) -> Result<YtDlpTopology, YtDlpTopologyError> {
    let Some(primary_document) = topology_platform_identity_document(&primary_topology) else {
        return Ok(primary_topology);
    };
    let recovered_document = match recover(&primary_document) {
        Ok(Some(document)) => document,
        Err(YtDlpServiceError::Cancellation) => {
            return Err(YtDlpTopologyError::Cancellation);
        }
        // Recovery остаётся fail-open: неудача дополнительной попытки не
        // превращает уже валидную primary topology в отказ playlist Add URL.
        Ok(None) | Err(_) => return Ok(primary_topology),
    };
    let Ok(recovered_json) = serde_json::to_vec(&recovered_document) else {
        return Ok(primary_topology);
    };
    if recovered_json.len() > budgets.json_line_bytes {
        return Ok(primary_topology);
    }
    match parse_topology_root(&recovered_json, budgets) {
        Ok(recovered @ YtDlpTopology::Video(_)) => Ok(recovered),
        Ok(_) | Err(_) => Ok(primary_topology),
    }
}

fn topology_platform_identity_document(topology: &YtDlpTopology) -> Option<Value> {
    let video = match topology {
        YtDlpTopology::Video(video) => video,
        YtDlpTopology::MultiVideo(multi_video) => multi_video.root_video(),
        YtDlpTopology::Playlist(_) | YtDlpTopology::Delegation(_) => return None,
    };
    let identity = video.identity();
    let extractor_key = identity.extractor_key()?;
    let (locator_field, locator) = if let Some(locator) = identity.webpage_locator() {
        ("webpage_url", locator)
    } else {
        ("original_url", identity.original_locator()?)
    };

    Some(json!({
        "extractor_key": extractor_key,
        (locator_field): locator.expose_secret_for_open(),
    }))
}

fn topology_from_process_output(
    process_output: TopologyProcessOutput,
    budgets: YtDlpTopologyBudgets,
) -> Result<YtDlpTopology, YtDlpTopologyError> {
    if !process_output.status.success() {
        return Err(YtDlpTopologyError::ExtractorRejection {
            stderr_bytes: process_output.stderr_bytes,
        });
    }

    let (root_json_line, lazy_json_lines) =
        process_output.stdout_lines.split_last().ok_or_else(|| {
            YtDlpTopologyError::invalid(YtDlpTopologyInvalidResponseReason::MissingJsonOutput)
        })?;
    validate_lazy_json_lines(lazy_json_lines, budgets)?;
    parse_topology_root(root_json_line, budgets)
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;

    use super::*;
    use crate::parse_yt_dlp_media_locator;

    #[test]
    fn disabled_adapter_and_invalid_budgets_fail_before_process_spawn() {
        let locator = parse_yt_dlp_media_locator("https://input.invalid/root")
            .expect("test locator должен быть valid");
        let disabled_config = YtDlpConfig {
            enabled: false,
            ..YtDlpConfig::default()
        };
        let disabled_error =
            extract_yt_dlp_topology_with_config(&locator, &disabled_config, || false)
                .expect_err("disabled adapter должен быть rejected");
        assert!(matches!(
            disabled_error,
            YtDlpTopologyError::AdapterDisabled
        ));

        let budget_error = extract_yt_dlp_topology_with_budgets(
            &locator,
            &YtDlpConfig::default(),
            YtDlpTopologyBudgets {
                stdout_bytes: 0,
                ..YtDlpTopologyBudgets::default()
            },
            || false,
        )
        .expect_err("zero stdout budget должен быть rejected");
        assert!(matches!(
            budget_error,
            YtDlpTopologyError::InvalidBudgets {
                field: YtDlpTopologyBudgetField::StdoutBytes
            }
        ));
    }

    #[test]
    fn nonzero_and_malformed_final_output_are_typed() {
        let nonzero_error = topology_from_process_output(
            TopologyProcessOutput {
                status: std::process::ExitStatus::from_raw(7 << 8),
                stdout_lines: Vec::new(),
                stderr_bytes: 42,
            },
            YtDlpTopologyBudgets::default(),
        )
        .expect_err("nonzero status должен быть extractor rejection");
        assert!(matches!(
            nonzero_error,
            YtDlpTopologyError::ExtractorRejection { stderr_bytes: 42 }
        ));

        let malformed_error = topology_from_process_output(
            TopologyProcessOutput {
                status: std::process::ExitStatus::from_raw(0),
                stdout_lines: vec![b"{malformed".to_vec()],
                stderr_bytes: 0,
            },
            YtDlpTopologyBudgets::default(),
        )
        .expect_err("malformed root JSON должен быть rejected");
        assert!(matches!(
            malformed_error,
            YtDlpTopologyError::InvalidExtractorResponse {
                reason: YtDlpTopologyInvalidResponseReason::MalformedJson
            }
        ));
    }

    #[test]
    fn video_topology_invokes_shared_hijack_recovery_path() {
        let primary = parse_topology_root(
            serde_json::to_vec(&json!({
                "id": "trailer",
                "extractor_key": "Youtube",
                "webpage_url": "https://www.youtube.com/watch?v=trailer",
                "title": "Trailer",
                "url": "https://media.invalid/trailer"
            }))
            .unwrap()
            .as_slice(),
            YtDlpTopologyBudgets::default(),
        )
        .expect("primary trailer topology");
        let mut invoked = false;

        let recovered = recover_topology_after_platform_hijack(
            primary,
            YtDlpTopologyBudgets::default(),
            |primary_document| {
                invoked = true;
                assert_eq!(
                    primary_document
                        .get("extractor_key")
                        .and_then(Value::as_str),
                    Some("Youtube")
                );
                Ok(Some(json!({
                    "id": "film",
                    "extractor_key": "Generic",
                    "webpage_url": "https://player.example/vod/42",
                    "title": "Catalog film",
                    "url": "https://media.invalid/film"
                })))
            },
        )
        .expect("recovered topology");

        assert!(invoked);
        assert_eq!(recovered.kind(), YtDlpTopologyKind::Video);
        assert_eq!(
            recovered
                .as_video()
                .and_then(|video| video.summary().title()),
            Some("Catalog film")
        );
    }
}
