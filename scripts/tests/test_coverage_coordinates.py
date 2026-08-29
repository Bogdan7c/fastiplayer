"""Focused tests строгого LLVM JSON3.1 source-coordinate extractor-а."""

from __future__ import annotations

import copy
import json
import sys
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_ROOT = REPO_ROOT / "scripts"
FIXTURE_ROOT = SCRIPTS_ROOT / "tests/fixtures/coverage_stable"
sys.path.insert(0, str(SCRIPTS_ROOT))
sys.path.insert(0, str(FIXTURE_ROOT))

import coverage_coordinates as coordinates  # noqa: E402
from fixture_factory import build_report  # noqa: E402


def load_fixture(name: str):
    with (FIXTURE_ROOT / name).open(encoding="utf-8") as fixture_file:
        return json.load(fixture_file)


class CoverageCoordinateExtractionTests(unittest.TestCase):
    def setUp(self):
        self.temporary_directory = tempfile.TemporaryDirectory(prefix="coverage-coordinates-")
        self.repo_root = Path(self.temporary_directory.name)
        (self.repo_root / "Cargo.toml").write_text("[workspace]\nmembers=[]\n", encoding="utf-8")
        self.policy = load_fixture("policy.json")
        self.profile = load_fixture("profile.json")

    def tearDown(self):
        self.temporary_directory.cleanup()

    def extract(self, report=None, *, run_label="run-1"):
        return coordinates.extract_run_state(
            report or build_report(self.repo_root),
            self.policy,
            self.profile,
            self.repo_root,
            run_label,
        )

    def decoded_coordinates(self, state, metric):
        files = state["source_files"]["universe"]
        decoded = []
        for coordinate in state["stable_source"]["coordinates"][metric]["universe"]:
            if metric == "lines":
                decoded.append((files[coordinate[0]], coordinate[1]))
            elif metric == "functions":
                decoded.append((files[coordinate[0]], coordinate[1], coordinate[2]))
            else:
                decoded.append(
                    (
                        files[coordinate[0]],
                        coordinate[1],
                        coordinate[2],
                        files[coordinate[3]],
                        *coordinate[4:],
                    )
                )
        return decoded

    def test_segments_follow_llvm_wrapped_gap_and_skipped_line_semantics(self):
        state = self.extract()
        lines = self.decoded_coordinates(state, "lines")
        self.assertEqual(
            lines,
            [
                ("crates/alpha/src/lib.rs", 1),
                ("crates/alpha/src/lib.rs", 3),
                ("crates/alpha/src/lib.rs", 5),
                ("crates/alpha/src/lib.rs", 6),
                ("crates/alpha/src/lib.rs", 7),
                ("crates/alpha/src/lib.rs", 8),
                ("crates/alpha/src/lib.rs", 9),
                ("crates/shell/src/lib.rs", 2),
            ],
        )
        workspace = state["stable_source"]["domains"]["workspace"]["lines"]
        self.assertEqual(workspace["counts"], {"covered": 5, "total": 8})
        # Function-group line summary является отдельной legacy surface, а не oracle unique lines.
        self.assertEqual(
            state["legacy_report_only"]["cross_check"]["lines"],
            {
                "category": "source-lines-vs-function-summary",
                "derived": {"covered": 5, "total": 8},
                "llvm": {"covered": 7, "total": 9},
                "covered_delta": -2,
                "total_delta": -1,
            },
        )

    def test_monomorphs_group_by_definition_and_regions_use_definition_prefix(self):
        state = self.extract()
        functions = self.decoded_coordinates(state, "functions")
        self.assertEqual(
            functions,
            [
                ("crates/alpha/src/lib.rs", 10, 1),
                ("crates/alpha/src/lib.rs", 20, 1),
                ("crates/shell/src/lib.rs", 2, 1),
            ],
        )
        regions = self.decoded_coordinates(state, "regions")
        self.assertEqual(len(regions), 4)
        self.assertEqual(regions[0][:4], ("crates/alpha/src/lib.rs", 10, 1, "crates/alpha/src/lib.rs"))
        # Второй monomorph покрывает region, который первый instantiation не исполнил.
        self.assertEqual(
            state["stable_source"]["domains"]["workspace"]["regions"]["counts"],
            {"covered": 2, "total": 4},
        )
        self.assertNotIn("dependency", json.dumps(state, sort_keys=True))
        self.assertEqual(
            state["legacy_report_only"]["cross_check"]["functions"]["category"],
            "llvm-instantiation-group-exact",
        )

    def test_monomorph_region_topology_is_a_structural_union(self):
        report = build_report(self.repo_root)
        report["data"][0]["functions"][0]["regions"].append(
            [11, 9, 11, 12, 0, 0, 0, 0]
        )
        for summary in (
            report["data"][0]["files"][0]["summary"],
            report["data"][0]["totals"],
        ):
            summary["regions"]["count"] += 1
            summary["regions"]["notcovered"] += 1
        state = self.extract(report)
        self.assertEqual(
            state["stable_source"]["domains"]["workspace"]["regions"]["counts"],
            {"covered": 2, "total": 5},
        )

    def test_duplicate_code_regions_receive_stable_occurrence_ordinals(self):
        report = build_report(self.repo_root)
        duplicate = [11, 3, 11, 8, 1, 0, 0, 0]
        report["data"][0]["functions"][0]["regions"].append(duplicate)
        report["data"][0]["functions"][1]["regions"].append(
            [11, 3, 11, 8, 0, 0, 0, 0]
        )
        for summary in (
            report["data"][0]["files"][0]["summary"],
            report["data"][0]["totals"],
        ):
            summary["regions"]["count"] += 1
            summary["regions"]["covered"] += 1
        state = self.extract(report)
        duplicate_coordinates = [
            coordinate
            for coordinate in self.decoded_coordinates(state, "regions")
            if coordinate[4:8] == (11, 3, 11, 8)
        ]
        self.assertEqual(len(duplicate_coordinates), 2)
        self.assertEqual({coordinate[-1] for coordinate in duplicate_coordinates}, {0, 1})
        self.assertEqual(
            state["stable_source"]["domains"]["workspace"]["regions"]["counts"],
            {"covered": 3, "total": 5},
        )

    def test_state_is_compact_hashed_and_contains_no_absolute_paths(self):
        state = self.extract()
        rendered = coordinates.canonical_json(state)
        self.assertNotIn(str(self.repo_root), rendered)
        self.assertNotIn("manifest_path", rendered)
        self.assertEqual(state["state_hash"], coordinates.content_hash({key: value for key, value in state.items() if key != "state_hash"}))
        self.assertEqual(
            state["stable_source"]["domains"]["crate:alpha"]["lines"]["universe_ranges"],
            [[0, 7]],
        )

    def test_duplicate_instantiation_coverage_uses_max_not_sum_or_cartesian_identity(self):
        report = build_report(self.repo_root)
        report["data"][0]["functions"][0]["count"] = 2
        report["data"][0]["functions"][1]["count"] = coordinates.INT64_MAX - 1
        state = self.extract(report)
        self.assertEqual(
            state["stable_source"]["domains"]["crate:alpha"]["functions"]["counts"],
            {"covered": 1, "total": 2},
        )

    def test_corrupt_counts_paths_kinds_and_topology_fail_closed(self):
        mutations = {}
        boolean_count = build_report(self.repo_root)
        boolean_count["data"][0]["files"][0]["segments"][0][2] = True
        mutations["bool-as-int"] = boolean_count
        sentinel = build_report(self.repo_root)
        sentinel["data"][0]["functions"][0]["count"] = coordinates.INT64_MAX
        mutations["sentinel"] = sentinel
        negative = build_report(self.repo_root)
        negative["data"][0]["functions"][0]["regions"][0][4] = -1
        mutations["negative"] = negative
        unknown_kind = build_report(self.repo_root)
        unknown_kind["data"][0]["functions"][0]["regions"][0][7] = 99
        mutations["unknown-kind"] = unknown_kind
        outside = build_report(self.repo_root)
        outside["data"][0]["files"][0]["filename"] = "/outside/alpha.rs"
        mutations["outside-path"] = outside
        malformed = build_report(self.repo_root)
        malformed["data"][0]["files"][0]["segments"][0].pop()
        mutations["malformed-segment"] = malformed
        duplicate_location = build_report(self.repo_root)
        duplicate_location["data"][0]["files"][0]["segments"].insert(
            1, [1, 1, 0, True, False, False]
        )
        mutations["topology"] = duplicate_location
        for name, report in mutations.items():
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.extract(report)

    def test_duplicate_normalized_file_path_is_rejected(self):
        report = build_report(self.repo_root)
        duplicate = copy.deepcopy(report["data"][0]["files"][0])
        duplicate["filename"] = str(self.repo_root / "crates/alpha/src/../src/lib.rs")
        report["data"][0]["files"].append(duplicate)
        with self.assertRaisesRegex(ValueError, "duplicate normalized path"):
            self.extract(report)

    def test_function_and_region_universe_cross_checks_are_blocking(self):
        report = build_report(self.repo_root)
        extra_function = copy.deepcopy(report["data"][0]["functions"][2])
        extra_function["name"] = "alpha::other"
        extra_function["regions"] = [[30, 1, 30, 9, 0, 0, 0, 0]]
        report["data"][0]["functions"].append(extra_function)
        with self.assertRaisesRegex(ValueError, "function definition groups"):
            self.extract(report)
        report = build_report(self.repo_root)
        report["data"][0]["functions"][0]["regions"].append(
            [11, 9, 11, 12, 0, 0, 0, 0]
        )
        with self.assertRaisesRegex(ValueError, "CodeRegion universe"):
            self.extract(report)

    def test_file_summary_and_global_summary_must_match(self):
        report = build_report(self.repo_root)
        report["data"][0]["totals"]["lines"]["covered"] -= 1
        with self.assertRaisesRegex(ValueError, r"files\[\]\.summary"):
            self.extract(report)

    def test_main_view_and_definition_region_are_unambiguous(self):
        ambiguous = build_report(self.repo_root)
        ambiguous["data"][0]["functions"][0]["regions"].pop()
        with self.assertRaisesRegex(ValueError, "ровно один main view"):
            self.extract(ambiguous)

        expansion_first = build_report(self.repo_root)
        regions = expansion_first["data"][0]["functions"][0]["regions"]
        regions.insert(0, regions.pop())
        with self.assertRaisesRegex(ValueError, "first region"):
            self.extract(expansion_first)

    def test_root_summary_and_unsupported_profiles_are_fail_closed(self):
        unexpected_root = build_report(self.repo_root)
        unexpected_root["unexpected"] = True
        bad_summary = build_report(self.repo_root)
        bad_summary["data"][0]["totals"]["regions"]["notcovered"] += 1
        unsupported = build_report(self.repo_root)
        unsupported["data"][0]["files"][0]["expansions"].append({})
        unsupported_summary = build_report(self.repo_root)
        unsupported_summary["data"][0]["totals"]["branches"].update(
            {"count": 1, "covered": 0, "notcovered": 1}
        )
        duplicate_unmapped = build_report(self.repo_root)
        duplicate_unmapped["data"][0]["files"][0]["segments"].insert(
            9, [9, 1, 0, False, False, False]
        )
        for report in (
            unexpected_root,
            bad_summary,
            unsupported,
            unsupported_summary,
            duplicate_unmapped,
        ):
            with self.subTest(report=report), self.assertRaises(ValueError):
                self.extract(report)

    def test_policy_exclusions_are_canonical_and_classified(self):
        for excluded_path in ("../outside.rs", "/tmp/outside.rs", "crates/unknown/src/lib.rs"):
            policy = copy.deepcopy(self.policy)
            policy["excluded_source_paths"] = [excluded_path]
            with self.subTest(excluded_path=excluded_path), self.assertRaises(ValueError):
                coordinates.extract_run_state(
                    build_report(self.repo_root),
                    policy,
                    self.profile,
                    self.repo_root,
                    "run-1",
                )

    def test_profile_policy_and_run_label_provenance_are_exact(self):
        wrong_profile = copy.deepcopy(self.profile)
        wrong_profile["llvm_cov_version"] = "22.1.1"
        with self.assertRaisesRegex(ValueError, "22.1.2"):
            coordinates.extract_run_state(
                build_report(self.repo_root),
                self.policy,
                wrong_profile,
                self.repo_root,
                "run-1",
            )
        with self.assertRaisesRegex(ValueError, "run-label"):
            self.extract(run_label="bad label")


if __name__ == "__main__":
    unittest.main()
