"""Focused tests для atomic stable baseline v2 transition policy."""

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
import coverage_coordinate_model as coordinate_model  # noqa: E402
import coverage_stability as stability  # noqa: E402
from fixture_factory import build_report  # noqa: E402


def load_fixture(name: str):
    """Читает checked-in fixture без зависимости от current working directory."""

    with (FIXTURE_ROOT / name).open(encoding="utf-8") as fixture_file:
        return json.load(fixture_file)


class CoverageBaselineUpdateTests(unittest.TestCase):
    """Закрепляет exact previous→proposed baseline/ledger boundary."""

    def setUp(self):
        self.temporary_directory = tempfile.TemporaryDirectory(
            prefix="coverage-baseline-update-"
        )
        self.repo_root = Path(self.temporary_directory.name)
        (self.repo_root / "Cargo.toml").write_text(
            "[workspace]\nmembers=[]\n", encoding="utf-8"
        )
        self.policy = load_fixture("policy.json")
        self.profile = load_fixture("profile.json")
        self.empty_exceptions = load_fixture("measurement_exceptions_empty.json")

    def tearDown(self):
        self.temporary_directory.cleanup()

    def state(
        self,
        fixture_run: int,
        label: str,
        *,
        add_file: bool = False,
        extra_covered: bool = True,
    ):
        report = build_report(self.repo_root, run=fixture_run, add_file=add_file)
        if add_file and not extra_covered:
            report["data"][0]["files"][-1]["segments"][0][2] = 0
            report["data"][0]["files"][-1]["summary"]["lines"]["covered"] = 0
            report["data"][0]["totals"]["lines"]["covered"] -= 1
        return coordinates.extract_run_state(
            report, self.policy, self.profile, self.repo_root, label
        )

    def cohort(
        self,
        fixture_runs=(1, 2, 3),
        *,
        add_file: bool = False,
        extra_covered: bool = True,
    ):
        states = [
            self.state(
                fixture_run,
                f"run-{index}",
                add_file=add_file,
                extra_covered=extra_covered,
            )
            for index, fixture_run in enumerate(fixture_runs, start=1)
        ]
        return stability.intersect_runs(self.policy, states)[0]

    @staticmethod
    def baseline_from_cohort(cohort):
        return stability.bootstrap_baseline(
            cohort,
            load_fixture("legacy_baseline_v1.json"),
            load_fixture("legacy_exceptions_v1.json"),
        )

    @staticmethod
    def exact_transition_exceptions(previous_baseline, proposed_baseline):
        entries = []
        previous_domains = previous_baseline["stable_source"]["domains"]
        proposed_domains = proposed_baseline["stable_source"]["domains"]
        for domain_name in sorted(set(previous_domains) & set(proposed_domains)):
            for metric in coordinates.METRICS:
                previous = previous_domains[domain_name][metric]
                proposed = proposed_domains[domain_name][metric]
                if previous["universe_hash"] == proposed["universe_hash"]:
                    continue
                if not stability._ratio_decreased(
                    proposed["counts"], previous["counts"]
                ):
                    continue
                entries.append(
                    {
                        "domain": domain_name,
                        "metric": metric,
                        "previous": copy.deepcopy(previous["counts"]),
                        "allowed": copy.deepcopy(proposed["counts"]),
                        "previous_universe_hash": previous["universe_hash"],
                        "current_universe_hash": proposed["universe_hash"],
                        "reason": "Fixture доказывает exact baseline transition.",
                        "review_by": "2099-01-01",
                        "follow_up": "Заменить provenance при следующем update.",
                    }
                )
        return {"schema_version": 1, "measurement_exceptions": entries}

    @staticmethod
    def baseline_with_changed_legacy_provenance(cohort):
        changed_exceptions = load_fixture("legacy_exceptions_v1.json")
        changed_exceptions["exceptions"][0]["reason"] += " Diagnostic only."
        return stability.bootstrap_baseline(
            cohort,
            load_fixture("legacy_baseline_v1.json"),
            changed_exceptions,
        )

    def test_same_universe_loss_is_unexceptable_and_gain_passes(self):
        stable_baseline = self.baseline_from_cohort(self.cohort((1, 1, 1)))
        variable_baseline = self.baseline_from_cohort(self.cohort((1, 2, 3)))

        passed, report = stability.check_baseline_update(
            stable_baseline,
            self.empty_exceptions,
            variable_baseline,
            self.empty_exceptions,
        )
        self.assertFalse(passed)
        self.assertIn(
            "exact-stable-coordinate-loss",
            {entry["kind"] for entry in report["regressions"]},
        )

        passed, report = stability.check_baseline_update(
            variable_baseline,
            self.empty_exceptions,
            stable_baseline,
            self.empty_exceptions,
        )
        self.assertTrue(passed)
        self.assertEqual(report["regressions"], [])

    def test_cross_universe_loss_requires_exact_proposed_transition_ledger(self):
        previous = self.baseline_from_cohort(self.cohort((1, 1, 1)))
        proposed = self.baseline_from_cohort(
            self.cohort((1, 1, 1), add_file=True, extra_covered=False)
        )
        exact_exceptions = self.exact_transition_exceptions(previous, proposed)
        self.assertEqual(len(exact_exceptions["measurement_exceptions"]), 3)

        passed, report = stability.check_baseline_update(
            previous,
            self.empty_exceptions,
            proposed,
            self.empty_exceptions,
        )
        self.assertFalse(passed)
        self.assertTrue(report["source_files_changed"])

        passed, report = stability.check_baseline_update(
            previous,
            self.empty_exceptions,
            proposed,
            exact_exceptions,
        )
        self.assertTrue(passed)
        self.assertEqual(report["consumed_exception_count"], 3)

        wrong_hash = copy.deepcopy(exact_exceptions)
        wrong_hash["measurement_exceptions"][0]["current_universe_hash"] = (
            "sha256:" + "0" * 64
        )
        passed, report = stability.check_baseline_update(
            previous,
            self.empty_exceptions,
            proposed,
            wrong_hash,
        )
        self.assertFalse(passed)
        self.assertIn(
            "cross-universe-stable-ratio-decrease",
            {entry["kind"] for entry in report["regressions"]},
        )

        # Historical previous rows никогда не авторизуют proposed transition.
        passed, _ = stability.check_baseline_update(
            previous,
            exact_exceptions,
            proposed,
            self.empty_exceptions,
        )
        self.assertFalse(passed)

        expired_proposed = copy.deepcopy(exact_exceptions)
        expired_proposed["measurement_exceptions"][0]["review_by"] = "2000-01-01"
        with self.assertRaisesRegex(ValueError, "просрочена"):
            stability.check_baseline_update(
                previous,
                self.empty_exceptions,
                proposed,
                expired_proposed,
            )

    def test_stale_overbroad_history_and_exception_only_edits_are_rejected(self):
        previous = self.baseline_from_cohort(self.cohort((1, 1, 1)))
        proposed = self.baseline_from_cohort(
            self.cohort((1, 1, 1), add_file=True, extra_covered=False)
        )
        overbroad = self.exact_transition_exceptions(previous, proposed)
        unrelated = copy.deepcopy(overbroad["measurement_exceptions"][0])
        unrelated["domain"] = "workspace"
        unrelated["metric"] = "functions"
        for side, baseline in (("previous", previous), ("allowed", proposed)):
            unrelated[side] = copy.deepcopy(
                baseline["stable_source"]["domains"]["workspace"]["functions"][
                    "counts"
                ]
            )
        unrelated["previous_universe_hash"] = previous["stable_source"]["domains"][
            "workspace"
        ]["functions"]["universe_hash"]
        unrelated["current_universe_hash"] = proposed["stable_source"]["domains"][
            "workspace"
        ]["functions"]["universe_hash"]
        overbroad["measurement_exceptions"].append(unrelated)

        passed, report = stability.check_baseline_update(
            previous,
            self.empty_exceptions,
            proposed,
            overbroad,
        )
        self.assertFalse(passed)
        self.assertIn(
            "unused-proposed-measurement-exception",
            {entry["kind"] for entry in report["regressions"]},
        )

        passed, report = stability.check_baseline_update(
            previous,
            self.empty_exceptions,
            previous,
            overbroad,
        )
        self.assertFalse(passed)
        self.assertIn(
            "measurement-exception-history-changed-without-baseline-update",
            {entry["kind"] for entry in report["regressions"]},
        )

        reordered_history = copy.deepcopy(overbroad)
        reordered_history["measurement_exceptions"].reverse()
        passed, report = stability.check_baseline_update(
            previous,
            overbroad,
            previous,
            reordered_history,
        )
        self.assertTrue(passed)
        self.assertEqual(report["regressions"], [])

    def test_expired_previous_history_can_be_replaced_without_recovery_deadlock(self):
        previous = self.baseline_from_cohort(self.cohort((1, 1, 1)))
        proposed = self.baseline_from_cohort(
            self.cohort((1, 1, 1), add_file=True, extra_covered=False)
        )
        fresh_proposed = self.exact_transition_exceptions(previous, proposed)
        expired_history = copy.deepcopy(fresh_proposed)
        for entry in expired_history["measurement_exceptions"]:
            entry["review_by"] = "2000-01-01"

        passed, report = stability.check_baseline_update(
            previous,
            expired_history,
            proposed,
            fresh_proposed,
        )
        self.assertTrue(passed)
        self.assertEqual(report["previous_exception_count"], 3)

        # Без baseline transition expired history нельзя удалить отдельным PR.
        passed, report = stability.check_baseline_update(
            previous,
            expired_history,
            previous,
            self.empty_exceptions,
        )
        self.assertFalse(passed)
        self.assertIn(
            "measurement-exception-history-changed-without-baseline-update",
            {entry["kind"] for entry in report["regressions"]},
        )

    def test_variable_order_is_ignored_but_legacy_provenance_rewrite_is_rejected(self):
        first_cohort = self.cohort((1, 2, 3))
        previous = self.baseline_from_cohort(first_cohort)
        reordered = self.baseline_from_cohort(self.cohort((3, 2, 1)))
        passed, report = stability.check_baseline_update(
            previous,
            self.empty_exceptions,
            reordered,
            self.empty_exceptions,
        )
        self.assertTrue(passed)
        self.assertFalse(report["blocking_baseline_changed"])

        rewritten = self.baseline_with_changed_legacy_provenance(first_cohort)
        passed, report = stability.check_baseline_update(
            previous,
            self.empty_exceptions,
            rewritten,
            self.empty_exceptions,
        )
        self.assertFalse(passed)
        self.assertFalse(report["blocking_baseline_changed"])
        self.assertIn(
            "legacy-report-only-provenance-changed",
            {entry["kind"] for entry in report["regressions"]},
        )

    def test_exact_ratio_has_no_rounding_or_hidden_threshold(self):
        self.assertFalse(
            stability._ratio_decreased(
                {"stable": 1, "total": 3}, {"stable": 2, "total": 6}
            )
        )
        self.assertTrue(
            stability._ratio_decreased(
                {"stable": 333_333, "total": 1_000_000},
                {"stable": 1, "total": 3},
            )
        )

    def test_exception_schema_rejects_bool_float_duplicate_and_sensitive_path(self):
        for schema_version in (True, 1.0):
            with self.subTest(schema_version=schema_version), self.assertRaisesRegex(
                ValueError, "integer"
            ):
                stability.validate_measurement_exceptions(
                    {
                        "schema_version": schema_version,
                        "measurement_exceptions": [],
                    }
                )

        previous = self.baseline_from_cohort(self.cohort((1, 1, 1)))
        proposed = self.baseline_from_cohort(
            self.cohort((1, 1, 1), add_file=True, extra_covered=False)
        )
        exact = self.exact_transition_exceptions(previous, proposed)
        duplicate = copy.deepcopy(exact)
        duplicate["measurement_exceptions"].append(
            copy.deepcopy(duplicate["measurement_exceptions"][0])
        )
        with self.assertRaisesRegex(ValueError, "duplicate"):
            stability.validate_measurement_exceptions(duplicate)

        sensitive = copy.deepcopy(exact)
        sensitive["measurement_exceptions"][0]["reason"] = "/tmp/private-evidence"
        with self.assertRaisesRegex(ValueError, "абсолютный путь"):
            stability.validate_measurement_exceptions(sensitive)

    def test_cli_preserves_zero_one_two_exit_contract(self):
        old_cohort = self.cohort((1, 1, 1))
        previous = self.baseline_from_cohort(old_cohort)
        proposed = self.baseline_from_cohort(
            self.cohort((1, 1, 1), add_file=True, extra_covered=False)
        )
        exact = self.exact_transition_exceptions(previous, proposed)
        paths = {
            "previous": self.repo_root / "previous.json",
            "previous_exceptions": self.repo_root / "previous-exceptions.json",
            "proposed": self.repo_root / "proposed.json",
            "proposed_exceptions": self.repo_root / "proposed-exceptions.json",
        }
        coordinate_model.write_json_atomic(paths["previous"], previous)
        coordinate_model.write_json_atomic(
            paths["previous_exceptions"], self.empty_exceptions
        )
        coordinate_model.write_json_atomic(paths["proposed"], proposed)
        coordinate_model.write_json_atomic(paths["proposed_exceptions"], exact)
        arguments = [
            "check-baseline-update",
            "--previous-baseline",
            str(paths["previous"]),
            "--previous-measurement-exceptions",
            str(paths["previous_exceptions"]),
            "--proposed-baseline",
            str(paths["proposed"]),
            "--proposed-measurement-exceptions",
            str(paths["proposed_exceptions"]),
        ]
        self.assertEqual(stability.main(arguments), 0)

        # Self-hashed, schema-valid legacy rewrite является semantic exit 1.
        coordinate_model.write_json_atomic(
            paths["proposed"],
            self.baseline_with_changed_legacy_provenance(old_cohort),
        )
        coordinate_model.write_json_atomic(
            paths["proposed_exceptions"], self.empty_exceptions
        )
        self.assertEqual(stability.main(arguments), 1)

        coordinate_model.write_json_atomic(paths["proposed"], proposed)
        self.assertEqual(stability.main(arguments), 1)

        paths["proposed_exceptions"].write_text("{", encoding="utf-8")
        self.assertEqual(stability.main(arguments), 2)
        with self.assertRaises(SystemExit) as missing_argument:
            stability.parse_args(["check-baseline-update"])
        self.assertEqual(missing_argument.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
