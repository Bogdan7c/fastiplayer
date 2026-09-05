"""Функциональный export→extraction→union и fail-closed границы артефактов."""

import copy
import gzip
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
sys.path.insert(0, str(SCRIPTS / "tests/fixtures/coverage_stable"))

from coverage_coordinate_model import read_json, write_json_atomic
from coverage_coordinates import extract_run_state
from coverage_object_export import export_objects, split_objects
from coverage_object_union import combine_object_reports
from coverage_runner_support import sha256_file
from coverage_stability_schema import validate_run_state
from fixture_factory import build_report


class ObjectUnionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "Cargo.toml").write_text("[workspace]\n")
        fixtures = SCRIPTS / "tests/fixtures/coverage_stable"
        self.policy = read_json(fixtures / "policy.json")
        self.profile = read_json(fixtures / "profile.json")
        self.reports = [build_report(self.root, run=n) for n in (1, 2)]
        self.legacy = extract_run_state(self.reports[0], self.policy, self.profile, self.root, "run-1")

    def publish(self, reports):
        exports = []
        for index, report in enumerate(reports):
            path = self.root / f"object-{index}.json.gz"
            with gzip.open(path, "wt") as output:
                json.dump(report, output)
            exports.append({"object": f"binary-{index}", "object_sha256": "0" * 64,
                            "report": path.name, "sha256": sha256_file(path)})
        manifest = self.root / "manifest.json"
        write_json_atomic(manifest, {"schema_version": 1,
                                    "kind": "coverage-per-executable-exports",
                                    "source": {"sha256": "fixture"}, "profile_sha256": "0" * 64,
                                    "objects": [e["object"] for e in exports], "exports": exports})
        return manifest

    def combine(self, manifest):
        return combine_object_reports(self.legacy, manifest, self.policy,
                                      self.profile, self.root, "run-1")

    def test_executed_coordinates_survive_unexecuted_copy_in_either_order(self):
        first = self.combine(self.publish(self.reports))
        reverse = self.combine(self.publish(list(reversed(self.reports))))
        self.assertEqual(first, reverse)
        validate_run_state(first, self.policy)
        self.assertEqual(first["legacy_report_only"], self.legacy["legacy_report_only"])
        before = self.legacy["stable_source"]["domains"]["workspace"]
        after = first["stable_source"]["domains"]["workspace"]
        for metric in ("lines", "functions", "regions"):
            self.assertGreater(after[metric]["counts"]["covered"], before[metric]["counts"]["covered"])
            self.assertEqual(after[metric]["counts"]["total"], before[metric]["counts"]["total"])
            self.assertLess(after[metric]["counts"]["covered"], after[metric]["counts"]["total"])

    def test_executable_without_workspace_source_does_not_remove_other_coverage(self):
        empty = copy.deepcopy(self.reports[0])
        datum = empty["data"][0]
        datum["files"] = []
        datum["functions"] = []
        for metric in datum["totals"].values():
            for counter in metric:
                metric[counter] = 0
        with self.assertRaises(ValueError):
            extract_run_state(empty, self.policy, self.profile, self.root, "run-1")
        self.assertEqual(self.combine(self.publish([self.reports[0]])),
                         self.combine(self.publish([empty, self.reports[0]])))

    def test_duplicate_execution_does_not_inflate_coordinate_counts(self):
        self.assertEqual(self.combine(self.publish([self.reports[0]])),
                         self.combine(self.publish([self.reports[0], self.reports[0]])))

    def test_missing_modified_or_invalid_report_is_rejected(self):
        for mutation in ("missing", "modified", "invalid"):
            with self.subTest(mutation=mutation):
                manifest = self.publish(self.reports)
                path = self.root / "object-0.json.gz"
                if mutation == "missing":
                    path.unlink()
                elif mutation == "modified":
                    path.write_bytes(b"corrupt")
                else:
                    report = copy.deepcopy(self.reports[0])
                    report["version"] = "unknown"
                    manifest = self.publish([report])
                with self.assertRaises((OSError, ValueError)):
                    self.combine(manifest)

    def test_missing_or_duplicate_object_set_is_rejected(self):
        for entries in ([], ["same", "same"]):
            manifest = self.publish(self.reports)
            payload = read_json(manifest)
            payload["exports"] = payload["exports"][:len(entries)]
            for entry, name in zip(payload["exports"], entries):
                entry["object"] = name
            write_json_atomic(manifest, payload)
            with self.assertRaises(ValueError):
                self.combine(manifest)

    def test_source_inventory_mismatch_is_rejected(self):
        self.legacy["source_files"]["universe"].append("crates/alpha/src/absent.rs")
        with self.assertRaisesRegex(ValueError, "source inventory"):
            self.combine(self.publish(self.reports))

    def test_adapter_to_union_preserves_hits_with_conflicting_export_order(self):
        # Fake LLVM моделирует доказанный first-object-wins defect; каждый single
        # export остаётся полноценным валидным LLVM JSON со своими counters.
        llvm = self.root / "llvm-cov"
        llvm.write_text(
            "#!/usr/bin/env python3\nimport pathlib,sys\n"
            "args=sys.argv[1:]\n"
            "binary=args[args.index('-object')+1]\n"
            "sys.stdout.write(pathlib.Path(binary).read_text())\n"
        )
        llvm.chmod(0o755)
        binaries = []
        for index, report in enumerate(self.reports):
            binary = self.root / f"binary-{index}"
            binary.write_text(json.dumps(report))
            binaries.append(binary)
        profile_path = self.root / "merged.profdata"
        profile_path.write_bytes(b"frozen profile")
        entries = [{"path": p.name, "sha256": sha256_file(p)} for p in binaries]
        results = []
        for index, order in enumerate([binaries, list(reversed(binaries))]):
            directory = self.root / f"exports-{index}"
            config = self.root / f"config-{index}.json"
            write_json_atomic(config, {
                "llvm_cov": str(llvm), "profile_directory": str(self.root),
                "output_directory": str(directory), "executables": entries,
                "source": {"sha256": "frozen-source"},
            })
            execution = subprocess.run(
                [sys.executable, str(SCRIPTS / "coverage_object_export.py"), "export",
                 "-instr-profile=" + str(profile_path),
                 *[arg for binary in order for arg in ["-object", str(binary)]]],
                env=dict(os.environ, RUSTIPLAYER_COVERAGE_EXPORT_CONFIG=str(config)),
                capture_output=True, text=True, check=True,
            )
            legacy = extract_run_state(json.loads(execution.stdout), self.policy,
                                       self.profile, self.root, "run-1")
            result = combine_object_reports(legacy, directory / "manifest.json",
                                            self.policy, self.profile, self.root, "run-1")
            results.append(result["stable_source"])
        self.assertEqual(results[0], results[1])
        self.assertGreater(results[0]["domains"]["workspace"]["functions"]["counts"]["covered"],
                           self.legacy["stable_source"]["domains"]["workspace"]["functions"]["counts"]["covered"])
        # Несуществующий/изменённый object запрещён до получения qualified результата.
        binaries[0].write_text("changed binary")
        config_document = read_json(config)
        config_document["output_directory"] = str(self.root / "rejected-export")
        with self.assertRaisesRegex(ValueError, "frozen inventory"):
            export_objects(config_document, ["export", "-instr-profile=" + str(profile_path),
                                             "-object", str(binaries[0])])

    def test_object_argument_parser_preserves_filters(self):
        common, objects = split_objects(["export", "-object", "one", "-object=two",
                                         "-ignore-filename-regex=tests", "-instr-profile=x"])
        self.assertEqual(objects, ["one", "two"])
        self.assertEqual(common, ["export", "-ignore-filename-regex=tests", "-instr-profile=x"])
        for invalid in (["export"], ["-object"], ["-object=x", "-object", "x"]):
            with self.assertRaises(ValueError):
                split_objects(invalid)


if __name__ == "__main__":
    unittest.main()
