//! Service-owned bounded extraction для `video/playlist/multi_video/url` topology.

mod limits;
mod model;
mod parser;
#[cfg(test)]
#[path = "parser/tests.rs"]
mod parser_tests;
mod process;

use rustiplayer_config::YtDlpConfig;

pub use limits::{
    DEFAULT_TOPOLOGY_DEPTH, DEFAULT_TOPOLOGY_ENTRY_COUNT, DEFAULT_TOPOLOGY_JSON_DEPTH,
    DEFAULT_TOPOLOGY_JSON_LINE_BYTES, DEFAULT_TOPOLOGY_STDERR_BYTES, DEFAULT_TOPOLOGY_STDOUT_BYTES,
    TOPOLOGY_IDENTITY_MAX_UTF8_BYTES, TOPOLOGY_LOCATOR_MAX_UTF8_BYTES,
    TOPOLOGY_METADATA_MAX_UTF8_BYTES, YtDlpTopologyBudgetField, YtDlpTopologyBudgets,
    YtDlpTopologyError, YtDlpTopologyInvalidResponseReason,
};
pub use model::{
    YtDlpDelegationMetadataPolicy, YtDlpTopology, YtDlpTopologyCollection, YtDlpTopologyDelegation,
    YtDlpTopologyEntry, YtDlpTopologyEntryKind, YtDlpTopologyIdentity, YtDlpTopologyKind,
    YtDlpTopologyMetadata, YtDlpTopologyMultiVideo, YtDlpTopologyVideo,
    YtDlpUnavailableTopologyEntry, YtDlpUnavailableTopologyReason,
};

use crate::YtDlpMediaLocator;
use crate::process::YtDlpProcessConfig;
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
        process_config.extraction_timeout(),
        budgets,
        &is_cancelled,
    )?;
    topology_from_process_output(process_output, budgets)
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
}
