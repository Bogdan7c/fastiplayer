//! Focused parser fixtures для S00/S15 topology contract.

use std::time::Duration;

use serde_json::{Value, json};

use super::model::YtDlpTopologySummaryFieldValue;
use super::parser::*;
use super::*;

#[test]
fn missing_type_is_video_and_ephemeral_format_url_is_not_retained() {
    let payload = json!({
        "id": "video-id",
        "title": "Video title",
        "formats": [{
            "format_id": "one",
            "url": "https://signed.invalid/video?token=secret"
        }]
    });
    let topology = parse_topology_root(
        serde_json::to_string(&payload).unwrap().as_bytes(),
        YtDlpTopologyBudgets::default(),
    )
    .expect("video fixture должна пройти");

    let video = topology.as_video().expect("ожидался video");
    assert_eq!(video.identity().extractor_id(), Some("video-id"));
    assert_eq!(video.summary().title(), Some("Video title"));
    assert!(!format!("{topology:?}").contains("secret"));
}

#[test]
fn rich_description_does_not_participate_in_playable_topology_summary() {
    // Двухбайтовый символ делает fixture длиннее production summary budget без огромного файла.
    let rich_description = "я".repeat((TOPOLOGY_SUMMARY_TEXT_MAX_UTF8_BYTES / 2) + 512);
    assert!(rich_description.len() > TOPOLOGY_SUMMARY_TEXT_MAX_UTF8_BYTES);
    let payload = json!({
        "id": "long-description-video",
        "title": "Playable title",
        "description": rich_description,
        "duration": 42.5,
        "formats": [{
            "format_id": "video",
            "url": "https://media.invalid/video"
        }]
    });

    // Проверяем пользовательскую функцию: playable root обязан пережить rich editorial metadata.
    let topology = parse_topology_root(
        serde_json::to_string(&payload).unwrap().as_bytes(),
        YtDlpTopologyBudgets::default(),
    )
    .expect("rich description не должна блокировать topology");
    let video = topology.as_video().expect("ожидался playable video root");

    assert_eq!(video.summary().title(), Some("Playable title"));
    assert_eq!(
        video.summary().title_state(),
        YtDlpTopologySummaryFieldState::Available
    );
    assert_eq!(
        video.summary().duration(),
        Some(Duration::from_millis(42_500))
    );
}

#[test]
fn malformed_optional_summary_fields_are_typed_and_non_fatal() {
    // Title превышает compact-card budget, но locator/identity/stream остаются корректными.
    let oversized_title = "я".repeat((TOPOLOGY_SUMMARY_TEXT_MAX_UTF8_BYTES / 2) + 1);
    let payload = json!({
        "id": "invalid-summary-video",
        "title": oversized_title,
        "duration": -1,
        "url": "https://media.invalid/video"
    });

    let topology = parse_topology_root(
        serde_json::to_string(&payload).unwrap().as_bytes(),
        YtDlpTopologyBudgets::default(),
    )
    .expect("необязательные summary fields не должны блокировать topology");
    let summary = topology
        .as_video()
        .expect("ожидался playable video root")
        .summary();

    assert_eq!(summary.title(), None);
    assert_eq!(
        summary.title_state(),
        YtDlpTopologySummaryFieldState::Unavailable(
            YtDlpTopologySummaryUnavailableReason::FieldBudgetExceeded,
        )
    );
    assert_eq!(summary.duration(), None);
    assert_eq!(
        summary.duration_state(),
        YtDlpTopologySummaryFieldState::Unavailable(
            YtDlpTopologySummaryUnavailableReason::InvalidNumericValue,
        )
    );
}

#[test]
fn playlist_retains_unavailable_and_missing_identity_entries() {
    let payload = json!({
        "_type": "playlist",
        "id": "playlist-id",
        "title": "Playlist",
        "entries": [
            null,
            {"_type": "url", "id": "unavailable-id", "title": "Unavailable"},
            {
                "_type": "video",
                "id": "restricted-id",
                "title": "Restricted",
                "url": "https://media.invalid/restricted",
                "availability": "private"
            }
        ],
        "n_entries": 99_999_999
    });
    let topology = parse_topology_root(
        serde_json::to_string(&payload).unwrap().as_bytes(),
        YtDlpTopologyBudgets::default(),
    )
    .expect("playlist fixture должна пройти");

    let playlist = topology.as_playlist().expect("ожидался playlist");
    let reasons = playlist
        .iter_entries()
        .map(|entry| match entry {
            YtDlpTopologyEntry::Unavailable(unavailable) => unavailable.reason(),
            other => panic!("ожидалась unavailable entry, получена {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        reasons,
        vec![
            YtDlpUnavailableTopologyReason::NullEntry,
            YtDlpUnavailableTopologyReason::MissingDelegationTarget,
            YtDlpUnavailableTopologyReason::RestrictedAvailability,
        ]
    );
    assert_eq!(playlist.entry_count(), 3);
}

#[test]
fn multi_video_requires_root_video_fields_and_entries() {
    let valid_payload = json!({
        "_type": "multi_video",
        "id": "compound-id",
        "title": "Compound",
        "formats": [{"format_id": "summary"}],
        "entries": [{
            "id": "part-id",
            "title": "Part",
            "url": "https://media.invalid/part"
        }]
    });
    let valid_topology = parse_topology_root(
        serde_json::to_string(&valid_payload).unwrap().as_bytes(),
        YtDlpTopologyBudgets::default(),
    )
    .expect("valid multi_video должен пройти");
    assert_eq!(
        valid_topology
            .as_multi_video()
            .expect("ожидался multi_video")
            .entry_count(),
        1
    );

    let invalid_payload = json!({
        "_type": "multi_video",
        "id": "compound-id",
        "title": "Compound",
        "entries": []
    });
    let error = parse_topology_root(
        serde_json::to_string(&invalid_payload).unwrap().as_bytes(),
        YtDlpTopologyBudgets::default(),
    )
    .expect_err("multi_video без video source fields должен быть rejected");
    assert!(matches!(
        error,
        YtDlpTopologyError::InvalidExtractorResponse {
            reason: YtDlpTopologyInvalidResponseReason::MissingVideoSourceDescription
        }
    ));
}

#[test]
fn url_and_url_transparent_use_distinct_merge_policies() {
    let plain_payload = json!({
        "_type": "url",
        "url": "https://delegate.invalid/plain?secret=one",
        "title": "Wrapper title"
    });
    let transparent_payload = json!({
        "_type": "url_transparent",
        "url": "https://delegate.invalid/transparent?secret=two",
        "title": "Transparent title"
    });
    let resolved = YtDlpTopologySummary::new(
        YtDlpTopologySummaryFieldValue::Available("Resolved title".to_owned()),
        YtDlpTopologySummaryFieldValue::Missing,
    );

    let plain = parse_topology_root(
        serde_json::to_string(&plain_payload).unwrap().as_bytes(),
        YtDlpTopologyBudgets::default(),
    )
    .expect("plain delegation должна пройти");
    let transparent = parse_topology_root(
        serde_json::to_string(&transparent_payload)
            .unwrap()
            .as_bytes(),
        YtDlpTopologyBudgets::default(),
    )
    .expect("transparent delegation должна пройти");

    let plain = plain.as_delegation().expect("ожидалась delegation");
    let transparent = transparent
        .as_delegation()
        .expect("ожидалась transparent delegation");
    assert_eq!(
        plain.merge_resolved_summary(&resolved).title(),
        Some("Resolved title")
    );
    assert_eq!(
        transparent.merge_resolved_summary(&resolved).title(),
        Some("Transparent title")
    );
    assert!(!format!("{plain:?}").contains("secret"));
    assert!(!format!("{transparent:?}").contains("secret"));
}

#[test]
fn transparent_wrapper_preserves_resolved_summary_failure_when_it_has_no_fallback() {
    let wrapper_payload = json!({
        "_type": "url_transparent",
        "url": "https://delegate.invalid/transparent"
    });
    let wrapper = parse_topology_root(
        serde_json::to_string(&wrapper_payload).unwrap().as_bytes(),
        YtDlpTopologyBudgets::default(),
    )
    .expect("transparent wrapper должен пройти")
    .as_delegation()
    .expect("ожидалась delegation")
    .clone();
    let resolved = YtDlpTopologySummary::new(
        YtDlpTopologySummaryFieldValue::Unavailable(
            YtDlpTopologySummaryUnavailableReason::FieldBudgetExceeded,
        ),
        YtDlpTopologySummaryFieldValue::Missing,
    );

    let merged = wrapper.merge_resolved_summary(&resolved);

    assert_eq!(
        merged.title_state(),
        YtDlpTopologySummaryFieldState::Unavailable(
            YtDlpTopologySummaryUnavailableReason::FieldBudgetExceeded,
        )
    );
}

#[test]
fn nested_cycle_depth_and_entry_budgets_are_typed() {
    let cycle = json!({
        "_type": "playlist",
        "id": "same",
        "entries": [{"_type": "playlist", "id": "same", "entries": []}]
    });
    let cycle_error = parse_topology_root(
        serde_json::to_string(&cycle).unwrap().as_bytes(),
        YtDlpTopologyBudgets::default(),
    )
    .expect_err("active-stack cycle должен быть rejected");
    assert!(matches!(
        cycle_error,
        YtDlpTopologyError::InvalidExtractorResponse {
            reason: YtDlpTopologyInvalidResponseReason::DelegationCycle
        }
    ));

    let nested = json!({
        "_type": "playlist",
        "id": "root",
        "entries": [{"_type": "playlist", "id": "child", "entries": []}]
    });
    let depth_error = parse_topology_root(
        serde_json::to_string(&nested).unwrap().as_bytes(),
        YtDlpTopologyBudgets {
            topology_depth: 1,
            ..YtDlpTopologyBudgets::default()
        },
    )
    .expect_err("nested topology должен превысить depth budget");
    assert!(matches!(
        depth_error,
        YtDlpTopologyError::TopologyDepthExceeded
    ));

    let entries = json!({"_type": "playlist", "id": "root", "entries": [null, null]});
    let entry_error = parse_topology_root(
        serde_json::to_string(&entries).unwrap().as_bytes(),
        YtDlpTopologyBudgets {
            entry_count: 1,
            ..YtDlpTopologyBudgets::default()
        },
    )
    .expect_err("entry budget должен быть enforced");
    assert!(matches!(
        entry_error,
        YtDlpTopologyError::EntryBudgetExceeded
    ));
}

#[test]
fn json_depth_scanner_ignores_brackets_inside_strings() {
    validate_json_depth(br#"{"title":"[[[{{{","entries":[]}"#, 2)
        .expect("скобки внутри строки не меняют JSON depth");
    let error = validate_json_depth(br#"{"entries":[{"id":"x"}]}"#, 2)
        .expect_err("третья structural nesting должна быть rejected");
    assert!(matches!(error, YtDlpTopologyError::JsonDepthExceeded));
}

#[test]
fn pinned_s00_topology_fixtures_are_all_supported() {
    let fixture_text = include_str!(
        "../../../compatibility/2026.07.04/fixtures/official-synthetic/result-topologies.json"
    );
    let fixture_document: Value =
        serde_json::from_str(fixture_text).expect("S00 fixture JSON должен парситься");
    let fixtures = fixture_document
        .get("fixtures")
        .and_then(Value::as_array)
        .expect("S00 fixtures должны быть array");
    let observed_kinds = fixtures
        .iter()
        .map(|fixture| {
            let payload = fixture
                .get("payload")
                .expect("fixture должен иметь payload");
            parse_topology_root(
                serde_json::to_string(payload).unwrap().as_bytes(),
                YtDlpTopologyBudgets::default(),
            )
            .expect("S00 topology fixture должна пройти")
            .kind()
        })
        .collect::<Vec<_>>();

    assert_eq!(
        observed_kinds,
        vec![
            YtDlpTopologyKind::Video,
            YtDlpTopologyKind::Playlist,
            YtDlpTopologyKind::MultiVideo,
            YtDlpTopologyKind::Delegation,
            YtDlpTopologyKind::Delegation,
        ]
    );
}
