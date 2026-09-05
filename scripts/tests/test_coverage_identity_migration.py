"""Проверяет разрешённую смену identity и враждебные переходы через настоящий CLI."""

import copy
import json
import subprocess
import sys
import unittest
from pathlib import Path

from test_coverage_baseline_update import CoverageBaselineUpdateTests

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import coverage_coordinate_model as model
from coverage_identity_migration import migrate_baseline


class CoverageIdentityMigrationCliTests(unittest.TestCase):
    def setUp(self):
        self.fixture = CoverageBaselineUpdateTests()
        self.fixture.setUp()
        self.addCleanup(self.fixture.tearDown)
        original = self.fixture.baseline_from_cohort(self.fixture.cohort())
        self.previous, self.previous_policy = migrate_baseline(
            original, self.fixture.policy, 'formerplayer-core', 'alpha'
        )
        self.proposed, self.proposed_policy = migrate_baseline(
            self.previous, self.previous_policy, 'fastiplayer', 'formerplayer'
        )
        self.previous_ledger = copy.deepcopy(self.fixture.empty_exceptions)
        self.proposed_ledger = copy.deepcopy(self.previous_ledger)
        self.registry = self.registration()

    def registration(self):
        entry = {'proposed_name': 'fastiplayer'}
        for name in ('previous', 'proposed'):
            entry[f'{name}_baseline_hash'] = model.content_hash(getattr(self, name))
            for kind in ('policy', 'ledger'):
                entry[f'{name}_{kind}_hash'] = model.content_hash(getattr(self, f'{name}_{kind}'))
        return {'schema_version': 1, 'migrations': [entry]}

    def cli(self, *, registered=True):
        root = self.fixture.repo_root
        arguments = []
        documents = {
            'previous-baseline': self.previous, 'proposed-baseline': self.proposed,
            'previous-measurement-exceptions': self.previous_ledger,
            'proposed-measurement-exceptions': self.proposed_ledger,
        }
        if registered:
            documents.update({'identity-migrations': self.registry,
                              'previous-policy': self.previous_policy,
                              'proposed-policy': self.proposed_policy})
        for name, document in documents.items():
            path = root / f'{name}.json'
            path.write_text(json.dumps(document))
            arguments.extend([f'--{name}', str(path)])
        return subprocess.run(
            [sys.executable, str(Path(__file__).resolve().parents[1] / 'coverage_stability.py'),
             'check-baseline-update', *arguments], capture_output=True, text=True, check=False,
        )

    def rehash(self):
        self.proposed.pop('baseline_hash', None)
        self.proposed['baseline_hash'] = model.content_hash(self.proposed)
        self.registry = self.registration()

    def test_exact_registered_migration_passes_and_default_stays_strict(self):
        result = self.cli()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotEqual(self.cli(registered=False).returncode, 0)

    def test_one_lost_stable_coordinate_fails_even_with_approved_hashes(self):
        surface = self.proposed['stable_source']
        universe = surface['coordinates']['lines']['universe']
        files = self.proposed['source_files']['universe']
        lost = surface['domains']['crate:fastiplayer-core']['lines']['stable_ranges'][0][0]
        for domain in surface['domains'].values():
            entry = domain['lines']
            indices = [i for start, end in entry['stable_ranges'] for i in range(start, end) if i != lost]
            entry['stable_ranges'] = model.ranges(indices)
            entry['counts']['stable'] = len(indices)
            entry['stable_hash'] = model.content_hash([model.coordinate_identity('lines', universe[i], files) for i in indices])
        self.rehash()
        self.assertNotEqual(self.cli().returncode, 0)

    def test_counter_measurement_and_archive_mutations_fail(self):
        original = copy.deepcopy(self.proposed)
        for mutation in ('counter', 'measurement', 'archive'):
            with self.subTest(mutation=mutation):
                self.proposed = copy.deepcopy(original)
                if mutation == 'counter':
                    self.proposed['stable_source']['domains']['workspace']['lines']['counts']['stable'] += 1
                elif mutation == 'measurement':
                    self.proposed['provenance']['profile_manifest_hash'] = 'sha256:' + '1' * 64
                else:
                    self.proposed['legacy_report_only']['exceptions_hash'] = 'sha256:' + '1' * 64
                self.rehash()
                self.assertNotEqual(self.cli().returncode, 0)

    def test_reclassification_fails_even_with_approved_hashes(self):
        self.proposed_policy['blocking_crates'].remove('fastiplayer-core')
        self.proposed_policy['informational_crates'].append('fastiplayer-core')
        self.proposed['provenance']['policy_hash'] = model.content_hash(self.proposed_policy)
        self.rehash()
        self.assertNotEqual(self.cli().returncode, 0)

    def test_exception_change_fails(self):
        self.proposed_ledger['measurement_exceptions'].append({})
        self.registry = self.registration()
        self.assertNotEqual(self.cli().returncode, 0)

    def test_wrong_hash_and_ambiguous_registration_fail(self):
        for field in self.registry['migrations'][0]:
            if not field.endswith('_hash'):
                continue
            with self.subTest(field=field):
                self.registry = self.registration()
                self.registry['migrations'][0][field] = 'sha256:' + '0' * 64
                self.assertNotEqual(self.cli().returncode, 0)
        self.registry = self.registration()
        self.registry['migrations'] *= 2
        self.assertNotEqual(self.cli().returncode, 0)

    def test_ambiguous_owner_mapping_fails(self):
        # Самосогласованный иной owner всё равно не является сменой одного бренда.
        self.proposed, self.proposed_policy = migrate_baseline(
            self.proposed, self.proposed_policy, 'different-shell', 'shell'
        )
        self.registry = self.registration()
        self.assertNotEqual(self.cli().returncode, 0)


if __name__ == '__main__':
    unittest.main()
