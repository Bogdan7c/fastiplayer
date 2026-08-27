#!/usr/bin/env python3
"""Functional regressions exact HLS manifest-selection acceptance evidence."""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path


SCRIPTS_DIRECTORY = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS_DIRECTORY))

from playback_acceptance_parser import PlaybackAcceptanceAnalyzer  # noqa: E402


def hls_marker(**overrides: object) -> str:
    """Воспроизводит точный порядок полей Rust `Display` formatter-а."""

    fields: dict[str, object] = {
        "phase": "final_receipt",
        "component_role": "video",
        "manifest_selection_id": 1,
        "landing_policy": "prefer_post_target_rap",
        "source_generation": 17,
        "requested_target_ms": 12_345,
        "actual_anchor_ms": 12_600,
        "actual_decode_anchor_ms": 12_400,
        "anchor_kind": "video_random_access_point",
        "media_sequence": 91,
        "discontinuity_sequence": 7,
        "manifest_segment_index": 5,
        "epoch_index": 2,
        "restart_segment_index": 3,
        "segment_start_ms": 12_000,
        "segment_end_ms": 18_000,
    }
    fields.update(overrides)
    ordered_names = (
        "phase",
        "component_role",
        "manifest_selection_id",
        "landing_policy",
        "source_generation",
        "requested_target_ms",
        "actual_anchor_ms",
        "actual_decode_anchor_ms",
        "anchor_kind",
        "media_sequence",
        "discontinuity_sequence",
        "manifest_segment_index",
        "epoch_index",
        "restart_segment_index",
        "segment_start_ms",
        "segment_end_ms",
    )
    rendered_fields = " ".join(
        f"{field_name}={fields[field_name]}" for field_name in ordered_names
    )
    return f"INFO kind=hls_manifest_segment_seek {rendered_fields}"


class HlsManifestSelectionAcceptanceTests(unittest.TestCase):
    """Проверяет HLS-owned records, validation, privacy и additive report."""

    def test_clean_muxed_av_cold_warm_post_target_and_discontinuity_records(self):
        analyzer = PlaybackAcceptanceAnalyzer(scenario="hls-exact-selections")
        analyzer.parse_lines(
            [
                hls_marker(
                    phase="initial_open",
                    component_role="muxed",
                    manifest_selection_id=1,
                    landing_policy="decode_from_or_before_target",
                    source_generation=1,
                    requested_target_ms=0,
                    actual_anchor_ms=500,
                    actual_decode_anchor_ms=500,
                    anchor_kind="audio_packet",
                    media_sequence=30,
                    discontinuity_sequence=0,
                    manifest_segment_index=0,
                    epoch_index=0,
                    restart_segment_index=0,
                    segment_start_ms=0,
                    segment_end_ms=6_000,
                ),
                hls_marker(
                    phase="initial_restore",
                    component_role="video",
                    manifest_selection_id=2,
                ),
                hls_marker(
                    phase="initial_restore",
                    component_role="audio",
                    manifest_selection_id=3,
                    actual_anchor_ms=12_360,
                    actual_decode_anchor_ms=12_300,
                    anchor_kind="audio_packet",
                ),
                hls_marker(
                    phase="preview",
                    component_role="video",
                    manifest_selection_id=4,
                    requested_target_ms=355_000,
                    actual_anchor_ms=360_100,
                    actual_decode_anchor_ms=360_000,
                    media_sequence=149,
                    discontinuity_sequence=9,
                    manifest_segment_index=77,
                    epoch_index=5,
                    restart_segment_index=4,
                    segment_start_ms=360_000,
                    segment_end_ms=366_000,
                ),
                hls_marker(
                    phase="final_receipt",
                    component_role="video",
                    manifest_selection_id=5,
                    requested_target_ms=355_000,
                    actual_anchor_ms=360_100,
                    actual_decode_anchor_ms=360_000,
                    media_sequence=149,
                    discontinuity_sequence=9,
                    manifest_segment_index=77,
                    epoch_index=5,
                    restart_segment_index=4,
                    segment_start_ms=360_000,
                    segment_end_ms=366_000,
                ),
            ],
            "hls-exact.log",
        )

        self.assertEqual(len(analyzer.hls_manifest_selections), 5)
        self.assertEqual(analyzer.hls_manifest_selection_anomalies, [])
        self.assertFalse(analyzer.has_proof_relevant_anomalies())
        cold_muxed = analyzer.hls_manifest_selections[0]
        self.assertEqual(cold_muxed.operation_class(), "cold")
        self.assertEqual((cold_muxed.segment_start_ms, cold_muxed.segment_end_ms), (0, 6_000))
        warm_final = analyzer.hls_manifest_selections[-1]
        self.assertEqual(warm_final.operation_class(), "warm")
        self.assertEqual(warm_final.requested_target_ms, 355_000)
        self.assertEqual(warm_final.actual_anchor_ms, 360_100)
        self.assertEqual(warm_final.discontinuity_sequence, 9)
        self.assertEqual(
            analyzer.hls_manifest_selections[2].actual_decode_anchor_ms,
            12_300,
        )

        summary_keys = {
            (
                row["operation_class"],
                row["phase"],
                row["component_role"],
            )
            for row in analyzer.hls_manifest_selection_summary_rows()
        }
        self.assertEqual(
            summary_keys,
            {
                ("cold", "initial_open", "muxed"),
                ("cold", "initial_restore", "video"),
                ("cold", "initial_restore", "audio"),
                ("warm", "preview", "video"),
                ("warm", "final_receipt", "video"),
            },
        )

    def test_duplicate_and_semantic_inconsistency_are_typed(self):
        analyzer = PlaybackAcceptanceAnalyzer(scenario="hls-semantic-anomalies")
        analyzer.parse_lines(
            [
                hls_marker(
                    component_role="muxed",
                    manifest_selection_id=0,
                    anchor_kind="audio_packet",
                    actual_decode_anchor_ms=12_600,
                ),
                hls_marker(manifest_selection_id=10),
                hls_marker(manifest_selection_id=10),
                hls_marker(
                    component_role="audio",
                    manifest_selection_id=20,
                    anchor_kind="video_random_access_point",
                ),
                hls_marker(
                    manifest_selection_id=11,
                    segment_start_ms=18_000,
                    segment_end_ms=18_000,
                ),
                hls_marker(
                    manifest_selection_id=12,
                    actual_anchor_ms=18_000,
                ),
            ],
            "hls-semantic-anomalies.log",
        )

        anomaly_kinds = [
            anomaly.kind for anomaly in analyzer.hls_manifest_selection_anomalies
        ]
        self.assertIn("zero_selection_id", anomaly_kinds)
        self.assertIn("duplicate_selection_id", anomaly_kinds)
        self.assertIn("anchor_kind_role_mismatch", anomaly_kinds)
        self.assertIn("invalid_segment_interval", anomaly_kinds)
        self.assertIn("actual_anchor_outside_segment", anomaly_kinds)
        self.assertTrue(analyzer.has_proof_relevant_anomalies())
        self.assertTrue(
            all(
                anomaly.proof_relevant()
                for anomaly in analyzer.hls_manifest_selection_anomalies
            )
        )

    def test_same_role_ids_may_commit_in_reverse_allocation_order(self):
        analyzer = PlaybackAcceptanceAnalyzer(scenario="hls-reverse-commit-order")
        analyzer.parse_lines(
            [
                hls_marker(manifest_selection_id=2),
                hls_marker(manifest_selection_id=1),
            ],
            "hls-reverse-commit-order.log",
        )

        self.assertEqual(
            [
                record.manifest_selection_id
                for record in analyzer.hls_manifest_selections
            ],
            [2, 1],
        )
        self.assertEqual(analyzer.hls_manifest_selection_anomalies, [])

    def test_duplicate_id_is_rejected_across_component_roles_in_one_source(self):
        analyzer = PlaybackAcceptanceAnalyzer(scenario="hls-cross-role-duplicate")
        analyzer.parse_lines(
            [
                hls_marker(manifest_selection_id=7, component_role="video"),
                hls_marker(
                    manifest_selection_id=7,
                    component_role="audio",
                    anchor_kind="audio_packet",
                ),
            ],
            "hls-cross-role-duplicate.log",
        )

        self.assertEqual(
            [
                anomaly.kind
                for anomaly in analyzer.hls_manifest_selection_anomalies
            ],
            ["duplicate_selection_id"],
        )

    def test_strict_decimal_and_known_enum_failures_never_raise_or_create_record(self):
        analyzer = PlaybackAcceptanceAnalyzer(scenario="hls-malformed")
        malformed_lines = [
            hls_marker(manifest_selection_id="1.5"),
            hls_marker(requested_target_ms="nan"),
            hls_marker(requested_target_ms="inf"),
            hls_marker(actual_anchor_ms="1e3"),
            hls_marker(actual_decode_anchor_ms="-1"),
            hls_marker(source_generation=18_446_744_073_709_551_616),
            hls_marker(phase="future_phase"),
            hls_marker(component_role="subtitle"),
            hls_marker(landing_policy="future_policy"),
            hls_marker(anchor_kind="future_anchor"),
            hls_marker(manifest_selection_id=13)
            + " manifest_selection_id=14",
            hls_marker(manifest_selection_id=15).replace(
                " segment_end_ms=18000", ""
            ),
        ]
        analyzer.parse_lines(malformed_lines, "hls-malformed.log")

        self.assertEqual(analyzer.hls_manifest_selections, [])
        anomaly_kinds = {
            anomaly.kind for anomaly in analyzer.hls_manifest_selection_anomalies
        }
        self.assertIn("invalid_unsigned_decimal", anomaly_kinds)
        self.assertIn("unsigned_decimal_overflow", anomaly_kinds)
        self.assertIn("unknown_phase", anomaly_kinds)
        self.assertIn("unknown_component_role", anomaly_kinds)
        self.assertIn("unknown_landing_policy", anomaly_kinds)
        self.assertIn("unknown_anchor_kind", anomaly_kinds)
        self.assertIn("duplicate_field", anomaly_kinds)
        self.assertIn("missing_field", anomaly_kinds)

    def test_adjacent_selection_ids_above_float_precision_remain_distinct(self):
        analyzer = PlaybackAcceptanceAnalyzer(scenario="hls-large-selection-ids")
        analyzer.parse_lines(
            [
                hls_marker(manifest_selection_id=9_007_199_254_740_992),
                hls_marker(manifest_selection_id=9_007_199_254_740_993),
            ],
            "hls-large-selection-ids.log",
        )

        self.assertEqual(
            [
                record.manifest_selection_id
                for record in analyzer.hls_manifest_selections
            ],
            [9_007_199_254_740_992, 9_007_199_254_740_993],
        )
        self.assertEqual(analyzer.hls_manifest_selection_anomalies, [])

    def test_report_excludes_secrets_and_never_joins_public_operation(self):
        analyzer = PlaybackAcceptanceAnalyzer(scenario="hls-privacy-no-join")
        public_target = 12_345
        analyzer.parse_lines(
            [
                "DEBUG Player command received command=Seek(SeekRequest { "
                f"target: Absolute(MediaTime({public_target}ms)), mode: Accurate }})",
                "DEBUG generation=44 target_ms=12345 Public final seek accepted",
                hls_marker(manifest_selection_id=30)
                + " uri=https://media.invalid/private.m3u8?token=TOPSECRET "
                "authorization=BearerSUPERSECRET scrub_command_id=999",
                hls_marker(
                    manifest_selection_id=31,
                    landing_policy="POLICY_SECRET_VALUE",
                ),
            ],
            "hls-privacy.log",
        )

        report = analyzer.to_dict()
        serialized = json.dumps(report, ensure_ascii=False)
        self.assertNotIn("TOPSECRET", serialized)
        self.assertNotIn("BearerSUPERSECRET", serialized)
        self.assertNotIn("POLICY_SECRET_VALUE", serialized)
        self.assertEqual(len(report["hls_manifest_selections"]), 1)
        selection_row = report["hls_manifest_selections"][0]
        self.assertNotIn("owner_seek_sequence", selection_row)
        self.assertNotIn("scrub_command_id", selection_row)
        self.assertNotIn("hls_manifest_selection", report["seek_samples"][0])
        self.assertEqual(
            report["hls_manifest_selection_anomalies"][0]["anomaly_kind"],
            "unknown_landing_policy",
        )

    def test_legacy_report_adds_empty_hls_sections_without_changing_existing_rows(self):
        analyzer = PlaybackAcceptanceAnalyzer(scenario="legacy-no-hls")
        analyzer.parse_lines(["DEBUG === rustiplayer ==="], "legacy.log")

        report = analyzer.to_dict()
        self.assertEqual(report["hls_manifest_selections"], [])
        self.assertEqual(report["hls_manifest_selection_anomalies"], [])
        self.assertEqual(report["hls_manifest_selection_summary"], [])
        self.assertEqual(
            report["hls_manifest_selection_anomaly_summary"],
            {
                "anomaly_count": 0,
                "proof_relevant_anomaly_count": 0,
                "by_kind": {},
            },
        )
        self.assertEqual(len(report["startup_runs"]), 1)
        self.assertIn("seek_samples", report)
        self.assertIn("network_requests", report)


if __name__ == "__main__":
    unittest.main()
