"""Tests exact three-run classification и cross-commit stable ratchet."""

from __future__ import annotations

import copy
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_ROOT = REPO_ROOT / "scripts"
FIXTURE_ROOT = SCRIPTS_ROOT / "tests/fixtures/coverage_stable"
sys.path.insert(0, str(SCRIPTS_ROOT))
sys.path.insert(0, str(FIXTURE_ROOT))

import coverage_coordinates as coordinates  # noqa: E402
import coverage_coordinate_model as coordinate_model  # noqa: E402
import coverage_legacy_schema as legacy_schema  # noqa: E402
import coverage_stability as stability  # noqa: E402
from fixture_factory import build_report  # noqa: E402


def load_fixture(name: str):
    with (FIXTURE_ROOT / name).open(encoding="utf-8") as fixture_file:
        return json.load(fixture_file)


class CoverageStabilityTests(unittest.TestCase):
    def setUp(self):
        self.temporary_directory = tempfile.TemporaryDirectory(prefix="coverage-stability-")
        self.repo_root = Path(self.temporary_directory.name)
        (self.repo_root / "Cargo.toml").write_text("[workspace]\nmembers=[]\n", encoding="utf-8")
        self.policy = load_fixture("policy.json")
        self.profile = load_fixture("profile.json")
        self.empty_exceptions = load_fixture("measurement_exceptions_empty.json")

    def tearDown(self):
        self.temporary_directory.cleanup()

    def state(self, fixture_run: int, label: str, *, add_file=False, extra_covered=True):
        report = build_report(self.repo_root, run=fixture_run, add_file=add_file)
        if add_file and not extra_covered:
            report["data"][0]["files"][-1]["segments"][0][2] = 0
            report["data"][0]["files"][-1]["summary"]["lines"]["covered"] = 0
            report["data"][0]["totals"]["lines"]["covered"] -= 1
        return coordinates.extract_run_state(
            report, self.policy, self.profile, self.repo_root, label
        )

    def cohort(self, fixture_runs=(1, 2, 3), *, add_file=False, extra_covered=True):
        states = [
            self.state(
                fixture_run,
                f"run-{index}",
                add_file=add_file,
                extra_covered=extra_covered,
            )
            for index, fixture_run in enumerate(fixture_runs, start=1)
        ]
        return stability.intersect_runs(self.policy, states), states

    @staticmethod
    def rehash(document, hash_name):
        payload = dict(document)
        payload.pop(hash_name)
        document[hash_name] = coordinates.content_hash(payload)

    def shell_two_line_report(self, *, first_covered: bool, add_file: bool):
        report = build_report(self.repo_root, run=1, add_file=add_file)
        shell = report["data"][0]["files"][1]
        shell["segments"] = [
            [2, 1, int(first_covered), True, True, False],
            [2, 8, 0, False, False, False],
            [4, 1, int(not first_covered), True, True, False],
            [4, 8, 0, False, False, False],
        ]
        shell["summary"]["lines"].update(
            {"count": 2, "covered": 1, "percent": 50.0}
        )
        report["data"][0]["totals"]["lines"]["count"] += 1
        report["data"][0]["totals"]["lines"]["covered"] += 1
        return report

    def test_exact_three_run_classification_and_variable_diagnostics(self):
        (cohort, diagnostics), _ = self.cohort()
        stability.validate_cohort(cohort)
        workspace = cohort["stable_source"]["domains"]["workspace"]
        self.assertEqual(
            workspace["lines"]["counts"],
            {"stable": 1, "variable": 5, "uncovered": 2, "total": 8},
        )
        self.assertEqual(
            workspace["functions"]["counts"],
            {"stable": 1, "variable": 1, "uncovered": 1, "total": 3},
        )
        self.assertEqual(
            workspace["regions"]["counts"],
            {"stable": 2, "variable": 1, "uncovered": 1, "total": 4},
        )
        line_diagnostics = diagnostics["variables"]["workspace"]["lines"]
        self.assertEqual(len(line_diagnostics), 5)
        self.assertIn([False, True, True], [entry["hits"] for entry in line_diagnostics])
        self.assertIn([True, False, False], [entry["hits"] for entry in line_diagnostics])

    def test_cohort_is_order_independent_but_diagnostics_preserve_cli_order(self):
        (_, _), states = self.cohort()
        cohort_a, diagnostics_a = stability.intersect_runs(self.policy, states)
        cohort_b, diagnostics_b = stability.intersect_runs(
            self.policy, [states[2], states[0], states[1]]
        )
        self.assertEqual(cohort_a, cohort_b)
        self.assertEqual(diagnostics_a["run_order"], ["run-1", "run-2", "run-3"])
        self.assertEqual(diagnostics_b["run_order"], ["run-3", "run-1", "run-2"])
        self.assertNotEqual(diagnostics_a, diagnostics_b)

    def test_artifact_builders_own_nested_state_after_return(self):
        (_, _), states = self.cohort((1, 1, 1))
        cohort, _ = stability.intersect_runs(self.policy, states)
        cohort_bytes = coordinates.canonical_json(cohort)
        states[0]["provenance"]["profile"] = "mutated"
        states[0]["source_files"]["universe"][0] = "crates/mutated/src/lib.rs"
        states[0]["stable_source"]["coordinates"]["lines"]["universe"][0][1] = 999
        self.assertEqual(coordinates.canonical_json(cohort), cohort_bytes)
        stability.validate_cohort(cohort)

        legacy_baseline = load_fixture("legacy_baseline_v1.json")
        legacy_exceptions = load_fixture("legacy_exceptions_v1.json")
        baseline = stability.bootstrap_baseline(
            cohort, legacy_baseline, legacy_exceptions
        )
        baseline_bytes = coordinates.canonical_json(baseline)
        cohort["provenance"]["profile"] = "mutated"
        cohort["source_files"]["universe"][0] = "crates/mutated/src/lib.rs"
        cohort["stable_source"]["domains"]["workspace"]["lines"][
            "universe_ranges"
        ].clear()
        legacy_baseline["workspace"]["lines"]["covered"] = 0
        legacy_exceptions["exceptions"][0]["reason"] = "mutated"
        self.assertEqual(coordinates.canonical_json(baseline), baseline_bytes)
        stability.validate_baseline(baseline)

    def test_intersect_rejects_coordinate_universe_or_provenance_mismatch(self):
        (_, _), states = self.cohort()
        changed_universe = self.state(3, "run-3", add_file=True)
        with self.assertRaisesRegex(ValueError, "source file universe"):
            stability.intersect_runs(self.policy, [states[0], states[1], changed_universe])
        changed_provenance = copy.deepcopy(states[2])
        changed_provenance["provenance"]["profile_manifest_hash"] = "sha256:" + "0" * 64
        changed_provenance["state_hash"] = coordinates.content_hash(
            {key: value for key, value in changed_provenance.items() if key != "state_hash"}
        )
        with self.assertRaisesRegex(ValueError, "provenance"):
            stability.intersect_runs(self.policy, [states[0], states[1], changed_provenance])

    def test_run_validation_rejects_corrupt_rle_hash_and_out_of_range_coordinate(self):
        state = self.state(1, "run-1")
        corruptions = []
        bad_rle = copy.deepcopy(state)
        bad_rle["stable_source"]["domains"]["workspace"]["lines"]["covered_ranges"] = [
            [0, 1],
            [1, 2],
        ]
        corruptions.append(("RLE", bad_rle))
        bad_hash = copy.deepcopy(state)
        bad_hash["stable_source"]["coordinates"]["lines"]["universe_hash"] = "sha256:" + "0" * 64
        corruptions.append(("SHA", bad_hash))
        bad_coordinate = copy.deepcopy(state)
        bad_coordinate["stable_source"]["coordinates"]["lines"]["universe"][0][0] = 99
        corruptions.append(("source file", bad_coordinate))
        for expected_error, corrupted in corruptions:
            with self.subTest(expected_error=expected_error), self.assertRaisesRegex(
                ValueError, expected_error
            ):
                stability.validate_run_state(corrupted, self.policy)

    def test_bootstrap_preserves_legacy_v1_counters_and_eight_identities(self):
        (cohort, _), _ = self.cohort()
        legacy_baseline = load_fixture("legacy_baseline_v1.json")
        legacy_exceptions = load_fixture("legacy_exceptions_v1.json")
        baseline = stability.bootstrap_baseline(cohort, legacy_baseline, legacy_exceptions)
        stability.validate_baseline(baseline)
        legacy = baseline["legacy_report_only"]
        self.assertEqual(legacy["baseline_v1"], legacy_baseline)
        self.assertEqual(len(legacy["exception_identities"]), 8)
        self.assertEqual(
            legacy["baseline_hash"], coordinates.content_hash(legacy_baseline)
        )

    def test_actual_legacy_lower_envelope_deltas_are_named_and_exact(self):
        baseline = coordinate_model.read_json(REPO_ROOT / "coverage/baseline.json")
        diagnostics = legacy_schema.lower_envelope_diagnostics(baseline)
        self.assertEqual(
            diagnostics,
            {
                "category": "independent-scope-lower-envelope-v1",
                "blocking_group_vs_crate_rows": {
                    "lines": {"covered_delta": 6, "total_delta": 0},
                    "functions": {"covered_delta": 0, "total_delta": 0},
                    "regions": {"covered_delta": 7, "total_delta": 0},
                },
                "workspace_vs_crate_rows": {
                    "lines": {"covered_delta": 6, "total_delta": 0},
                    "functions": {"covered_delta": 0, "total_delta": 0},
                    "regions": {"covered_delta": 10, "total_delta": 0},
                },
            },
        )

    def test_same_universe_exact_stable_loss_blocks_without_exception_escape(self):
        (baseline_cohort, _), _ = self.cohort((1, 1, 1))
        baseline = stability.bootstrap_baseline(
            baseline_cohort,
            load_fixture("legacy_baseline_v1.json"),
            load_fixture("legacy_exceptions_v1.json"),
        )
        (current_cohort, _), _ = self.cohort((1, 2, 3))
        passed, report = stability.check_baseline(
            baseline,
            current_cohort,
            self.empty_exceptions,
            allow_universe_update=True,
        )
        self.assertFalse(passed)
        self.assertIn(
            "exact-stable-coordinate-loss",
            {regression["kind"] for regression in report["regressions"]},
        )
        # Старый aggregate exception manifest имеет другой schema и не может дать обход.
        with self.assertRaises(ValueError):
            stability.check_baseline(
                baseline,
                current_cohort,
                load_fixture("legacy_exceptions_v1.json"),
                allow_universe_update=True,
            )

    def test_unchanged_stable_cohort_passes(self):
        (cohort, _), _ = self.cohort((1, 1, 1))
        baseline = stability.bootstrap_baseline(
            cohort,
            load_fixture("legacy_baseline_v1.json"),
            load_fixture("legacy_exceptions_v1.json"),
        )
        passed, report = stability.check_baseline(
            baseline, cohort, self.empty_exceptions, allow_universe_update=False
        )
        self.assertTrue(passed)
        self.assertEqual(report["status"], "pass")

    def test_new_file_requires_explicit_update_even_when_stable_ratio_improves(self):
        (old_cohort, _), _ = self.cohort((1, 1, 1))
        baseline = stability.bootstrap_baseline(
            old_cohort,
            load_fixture("legacy_baseline_v1.json"),
            load_fixture("legacy_exceptions_v1.json"),
        )
        (new_cohort, _), _ = self.cohort((1, 1, 1), add_file=True)
        passed_without_update, report = stability.check_baseline(
            baseline, new_cohort, self.empty_exceptions, allow_universe_update=False
        )
        self.assertFalse(passed_without_update)
        self.assertTrue(report["source_files_changed"])
        passed_with_update, _ = stability.check_baseline(
            baseline, new_cohort, self.empty_exceptions, allow_universe_update=True
        )
        self.assertTrue(passed_with_update)

    def test_deleted_file_requires_explicit_universe_update(self):
        (old_cohort, _), _ = self.cohort((1, 1, 1), add_file=True)
        baseline = stability.bootstrap_baseline(
            old_cohort,
            load_fixture("legacy_baseline_v1.json"),
            load_fixture("legacy_exceptions_v1.json"),
        )
        (current_cohort, _), _ = self.cohort((1, 1, 1), add_file=False)
        passed, report = stability.check_baseline(
            baseline,
            current_cohort,
            self.empty_exceptions,
            allow_universe_update=False,
        )
        self.assertFalse(passed)
        self.assertTrue(report["source_files_changed"])

    def test_cross_universe_ratio_loss_requires_exact_measurement_scoped_exceptions(self):
        (old_cohort, _), _ = self.cohort((1, 1, 1))
        baseline = stability.bootstrap_baseline(
            old_cohort,
            load_fixture("legacy_baseline_v1.json"),
            load_fixture("legacy_exceptions_v1.json"),
        )
        (new_cohort, _), _ = self.cohort(
            (1, 1, 1), add_file=True, extra_covered=False
        )
        passed, report = stability.check_baseline(
            baseline, new_cohort, self.empty_exceptions, allow_universe_update=True
        )
        self.assertFalse(passed)
        affected = {
            (entry["domain"], entry["metric"])
            for entry in report["regressions"]
            if entry["kind"] == "cross-universe-stable-ratio-decrease"
        }
        self.assertEqual(
            affected,
            {("workspace", "lines"), ("blocking-group", "lines"), ("crate:alpha", "lines")},
        )
        exception_entries = []
        for domain, metric in sorted(affected):
            previous = baseline["stable_source"]["domains"][domain][metric]
            current = new_cohort["stable_source"]["domains"][domain][metric]
            exception_entries.append(
                {
                    "domain": domain,
                    "metric": metric,
                    "previous": previous["counts"],
                    "allowed": {
                        "stable": current["counts"]["stable"],
                        "total": current["counts"]["total"],
                    },
                    "previous_universe_hash": previous["universe_hash"],
                    "current_universe_hash": current["universe_hash"],
                    "reason": "Проверяем exact measurement-scoped update boundary.",
                    "review_by": "2099-01-01",
                    "follow_up": "Удалить fixture exception после обновления baseline.",
                }
            )
        scoped_exceptions = {
            "schema_version": 1,
            "measurement_exceptions": exception_entries,
        }
        passed, report = stability.check_baseline(
            baseline, new_cohort, scoped_exceptions, allow_universe_update=True
        )
        self.assertTrue(passed)
        self.assertEqual(report["regressions"], [])

        wrong_hash = copy.deepcopy(scoped_exceptions)
        wrong_hash["measurement_exceptions"][0]["current_universe_hash"] = (
            "sha256:" + "0" * 64
        )
        passed, report = stability.check_baseline(
            baseline, new_cohort, wrong_hash, allow_universe_update=True
        )
        self.assertFalse(passed)
        self.assertTrue(report["regressions"])

        stale = copy.deepcopy(scoped_exceptions)
        stale["measurement_exceptions"].append(
            {
                **copy.deepcopy(stale["measurement_exceptions"][0]),
                "domain": "workspace",
                "metric": "functions",
                "previous": baseline["stable_source"]["domains"]["workspace"]["functions"]["counts"],
                "allowed": {
                    "stable": new_cohort["stable_source"]["domains"]["workspace"]["functions"]["counts"]["stable"],
                    "total": new_cohort["stable_source"]["domains"]["workspace"]["functions"]["counts"]["total"],
                },
            }
        )
        with self.assertRaisesRegex(ValueError, "stale/unused"):
            stability.check_baseline(
                baseline, new_cohort, stale, allow_universe_update=True
            )

        expired = copy.deepcopy(scoped_exceptions)
        expired["measurement_exceptions"][0]["review_by"] = "2000-01-01"
        with self.assertRaisesRegex(ValueError, "просрочена"):
            stability.check_baseline(
                baseline, new_cohort, expired, allow_universe_update=True
            )

        unknown_domain = copy.deepcopy(scoped_exceptions)
        unknown_domain["measurement_exceptions"][0]["domain"] = "crate:missing"
        with self.assertRaisesRegex(ValueError, "отсутствующие domains"):
            stability.check_baseline(
                baseline, new_cohort, unknown_domain, allow_universe_update=True
            )

    def test_global_index_shift_cannot_hide_same_domain_stable_replacement(self):
        policy = copy.deepcopy(self.policy)
        policy["blocking_crates"] = ["alpha", "shell"]
        policy["informational_crates"] = []

        def build_cohort(*, first_covered: bool, add_file: bool):
            states = [
                coordinates.extract_run_state(
                    self.shell_two_line_report(
                        first_covered=first_covered, add_file=add_file
                    ),
                    policy,
                    self.profile,
                    self.repo_root,
                    f"run-{index}",
                )
                for index in (1, 2, 3)
            ]
            return stability.intersect_runs(policy, states)[0]

        old_cohort = build_cohort(first_covered=True, add_file=False)
        current_cohort = build_cohort(first_covered=False, add_file=True)
        legacy_baseline = load_fixture("legacy_baseline_v1.json")
        legacy_baseline["blocking_crates"]["shell"] = legacy_baseline[
            "informational_crates"
        ].pop("shell")
        legacy_baseline["blocking_group"] = copy.deepcopy(legacy_baseline["workspace"])
        legacy_exceptions = load_fixture("legacy_exceptions_v1.json")
        for exception in legacy_exceptions["exceptions"]:
            if exception["scope"] == "blocking-group":
                exception["allowed"] = copy.deepcopy(
                    legacy_baseline["blocking_group"][exception["metric"]]
                )
                exception["previous"] = copy.deepcopy(exception["allowed"])
        baseline = stability.bootstrap_baseline(
            old_cohort, legacy_baseline, legacy_exceptions
        )
        old_shell = baseline["stable_source"]["domains"]["crate:shell"]["lines"]
        new_shell = current_cohort["stable_source"]["domains"]["crate:shell"]["lines"]
        self.assertEqual(old_shell["universe_hash"], new_shell["universe_hash"])
        self.assertNotEqual(old_shell["universe_ranges"], new_shell["universe_ranges"])
        passed, report = stability.check_baseline(
            baseline,
            current_cohort,
            self.empty_exceptions,
            allow_universe_update=True,
        )
        self.assertFalse(passed)
        self.assertIn(
            ("crate:shell", "lines"),
            {
                (entry["domain"], entry["metric"])
                for entry in report["regressions"]
                if entry["kind"] == "exact-stable-coordinate-loss"
            },
        )

    def test_domain_membership_is_reconstructed_from_coordinate_owners(self):
        (cohort, _), _ = self.cohort((1, 1, 1))
        corrupted = copy.deepcopy(cohort)
        workspace = corrupted["stable_source"]["domains"]["workspace"]["lines"]
        workspace.update(
            {
                "universe_ranges": [],
                "stable_ranges": [],
                "variable_ranges": [],
                "uncovered_ranges": [],
                "universe_hash": coordinates.content_hash([]),
                "stable_hash": coordinates.content_hash([]),
                "counts": {"stable": 0, "variable": 0, "uncovered": 0, "total": 0},
            }
        )
        self.rehash(corrupted, "cohort_hash")
        with self.assertRaisesRegex(ValueError, "полным universe"):
            stability.validate_cohort(corrupted)

    def test_versioned_artifact_hashes_ranges_paths_and_counters_fail_closed(self):
        state = self.state(1, "run-1")
        (cohort, _), _ = self.cohort((1, 1, 1))
        baseline = stability.bootstrap_baseline(
            cohort,
            load_fixture("legacy_baseline_v1.json"),
            load_fixture("legacy_exceptions_v1.json"),
        )
        corruptions = []
        bad_state_hash = copy.deepcopy(state)
        bad_state_hash["state_hash"] = "sha256:" + "0" * 64
        corruptions.append((stability.validate_run_state, bad_state_hash))
        bad_source_hash = copy.deepcopy(state)
        bad_source_hash["source_files"]["hash"] = "sha256:" + "0" * 64
        self.rehash(bad_source_hash, "state_hash")
        corruptions.append((stability.validate_run_state, bad_source_hash))
        bad_covered_hash = copy.deepcopy(state)
        bad_covered_hash["stable_source"]["domains"]["workspace"]["lines"][
            "covered_hash"
        ] = "sha256:" + "0" * 64
        self.rehash(bad_covered_hash, "state_hash")
        corruptions.append((stability.validate_run_state, bad_covered_hash))
        for invalid_position in (True, coordinates.INT64_MAX):
            bad_coordinate = copy.deepcopy(state)
            bad_coordinate["stable_source"]["coordinates"]["lines"]["universe"][0][
                1
            ] = invalid_position
            self.rehash(bad_coordinate, "state_hash")
            corruptions.append((stability.validate_run_state, bad_coordinate))
        absolute_path = copy.deepcopy(state)
        absolute_path["source_files"]["universe"][0] = "/tmp/private.rs"
        absolute_path["source_files"]["hash"] = coordinates.content_hash(
            absolute_path["source_files"]["universe"]
        )
        self.rehash(absolute_path, "state_hash")
        corruptions.append((stability.validate_run_state, absolute_path))
        for provenance_name, invalid_value in (
            ("profile", "partial"),
            ("llvm_cov_version", "22.1.1"),
            ("cargo_llvm_cov_version", "0.8.6"),
            ("profile_manifest_hash", "not-a-sha"),
        ):
            bad_provenance = copy.deepcopy(state)
            bad_provenance["provenance"][provenance_name] = invalid_value
            self.rehash(bad_provenance, "state_hash")
            corruptions.append((stability.validate_run_state, bad_provenance))
        bad_cohort_hash = copy.deepcopy(cohort)
        bad_cohort_hash["cohort_hash"] = "sha256:" + "0" * 64
        corruptions.append((stability.validate_cohort, bad_cohort_hash))
        bad_cohort_stable_hash = copy.deepcopy(cohort)
        bad_cohort_stable_hash["stable_source"]["domains"]["workspace"]["lines"][
            "stable_hash"
        ] = "sha256:" + "0" * 64
        self.rehash(bad_cohort_stable_hash, "cohort_hash")
        corruptions.append((stability.validate_cohort, bad_cohort_stable_hash))
        bad_state_hash_format = copy.deepcopy(cohort)
        bad_state_hash_format["run_set"][0]["state_hash"] = "not-a-sha"
        self.rehash(bad_state_hash_format, "cohort_hash")
        corruptions.append((stability.validate_cohort, bad_state_hash_format))
        bad_cohort_rle = copy.deepcopy(cohort)
        line_count = bad_cohort_rle["stable_source"]["domains"]["workspace"]["lines"][
            "counts"
        ]["total"]
        bad_cohort_rle["stable_source"]["domains"]["workspace"]["lines"][
            "universe_ranges"
        ] = [[0, 1], [1, line_count]]
        self.rehash(bad_cohort_rle, "cohort_hash")
        corruptions.append((stability.validate_cohort, bad_cohort_rle))
        bad_stable_hash = copy.deepcopy(baseline)
        bad_stable_hash["stable_source"]["domains"]["workspace"]["lines"][
            "stable_hash"
        ] = "sha256:" + "0" * 64
        self.rehash(bad_stable_hash, "baseline_hash")
        corruptions.append((stability.validate_baseline, bad_stable_hash))
        bad_baseline_rle = copy.deepcopy(baseline)
        baseline_line_count = bad_baseline_rle["stable_source"]["domains"]["workspace"][
            "lines"
        ]["counts"]["total"]
        bad_baseline_rle["stable_source"]["domains"]["workspace"]["lines"][
            "universe_ranges"
        ] = [[0, 1], [1, baseline_line_count]]
        self.rehash(bad_baseline_rle, "baseline_hash")
        corruptions.append((stability.validate_baseline, bad_baseline_rle))
        for validator, corrupted in corruptions:
            with self.subTest(validator=validator.__name__), self.assertRaises(ValueError):
                validator(corrupted)

    def test_bootstrap_rejects_malformed_legacy_inventory_and_exceptions(self):
        (cohort, _), _ = self.cohort((1, 1, 1))
        baseline = load_fixture("legacy_baseline_v1.json")
        exceptions = load_fixture("legacy_exceptions_v1.json")
        bad_counter = copy.deepcopy(baseline)
        bad_counter["workspace"]["lines"]["covered"] = 99
        with self.assertRaises(ValueError):
            stability.bootstrap_baseline(cohort, bad_counter, exceptions)
        bad_exception = copy.deepcopy(exceptions)
        bad_exception["exceptions"][0]["allowed"]["covered"] -= 1
        with self.assertRaisesRegex(ValueError, "allowed"):
            stability.bootstrap_baseline(cohort, baseline, bad_exception)
        expired = copy.deepcopy(exceptions)
        expired["exceptions"][0]["review_by"] = "2000-01-01"
        with self.assertRaisesRegex(ValueError, "просрочено"):
            stability.bootstrap_baseline(cohort, baseline, expired)

    def test_transactional_pair_restores_report_only_output_if_blocking_publish_fails(self):
        diagnostics_path = self.repo_root / "variable.json"
        cohort_path = self.repo_root / "cohort.json"
        diagnostics_path.write_text("old diagnostics\n", encoding="utf-8")
        cohort_path.write_text("old cohort\n", encoding="utf-8")
        real_replace = os.replace
        replace_calls = 0

        def fail_second_replace(source, destination):
            nonlocal replace_calls
            replace_calls += 1
            if replace_calls == 2:
                raise OSError("synthetic blocking publish failure")
            return real_replace(source, destination)

        with mock.patch.object(coordinate_model.os, "replace", side_effect=fail_second_replace):
            with self.assertRaisesRegex(OSError, "synthetic"):
                coordinate_model.write_json_pair_transactional(
                    diagnostics_path,
                    {"new": "diagnostics"},
                    cohort_path,
                    {"new": "cohort"},
                )
        self.assertEqual(diagnostics_path.read_text(), "old diagnostics\n")
        self.assertEqual(cohort_path.read_text(), "old cohort\n")


if __name__ == "__main__":
    unittest.main()
