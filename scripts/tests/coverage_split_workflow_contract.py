"""Проверяет разделение manual measurement и обязательной baseline policy.

Общий старый oracle остаётся владельцем строгих step/env/shell invariants.
Здесь проверяются реальные triggers/job owners, затем создаётся validation view
из их неизменённых тел: это не исполняемый workflow и не подмена CI config.
"""

import re

import coverage_workflow_contract as contract


def _job(document: str, name: str) -> str:
    """Извлекает один canonical job, не выбирая произвольный duplicate owner."""
    contract._require_canonical_mapping_keys(document, 0, 'workflow root')
    contract._require(contract._yaml_key_count(document, 0, 'jobs') == 1, 'one jobs owner required')
    jobs = '\n'.join(contract._indented_block_lines(document, 'jobs:', 2))
    contract._require_canonical_mapping_keys(jobs, 2, 'workflow jobs')
    contract._require(contract._yaml_key_count(jobs, 2, name) == 1, 'one selected job required')
    pattern = rf'(?ms)^  {re.escape(name)}:\n(?P<body>.*?)(?=^  [A-Za-z_][A-Za-z0-9_-]*:\n|\Z)'
    return re.search(pattern, jobs)['body']


def coverage_validation_document(main_workflow: str, manual_workflow: str) -> str:
    """Fail-closed проверяет split и возвращает projection для общих oracles."""
    contract._require('scripts/coverage.sh' not in main_workflow, 'ordinary CI must not run full coverage')
    for workflow in [main_workflow, manual_workflow]:
        contract._require(contract._yaml_key_count(workflow, 0, 'defaults') == 0, 'workflow shell defaults must not bypass step validation')
    contract._require(contract._yaml_key_count(manual_workflow, 0, 'on') == 1, 'manual trigger owner required')
    triggers = contract._indented_block_lines(manual_workflow, 'on:', 2)
    trigger_entries = tuple(line.strip() for line in triggers if line.strip() and not line.strip().startswith('#'))
    contract._require(trigger_entries == ('workflow_dispatch:',), 'full coverage must be manual only')

    baseline_job = _job(main_workflow, 'coverage-baseline-policy')
    contract._require_canonical_mapping_keys(baseline_job, 4, 'baseline policy job')
    contract._require(contract._yaml_key_count(baseline_job, 4, 'name') == 1, 'one baseline status name required')
    contract._require(baseline_job.count('    name: Coverage baseline policy\n') == 1, 'baseline status identity required')
    for key in ['if', 'continue-on-error', 'needs', 'strategy', 'env', 'defaults']:
        contract._require(contract._yaml_key_count(baseline_job, 4, key) == 0, 'baseline policy must always enforce failures')
    contract._require(contract._yaml_key_count(baseline_job, 4, 'steps') == 1, 'one baseline steps owner required')
    schema_step = contract._named_step_body(baseline_job, 'Validate tracked coverage policy')
    for key in ['if', 'continue-on-error', 'shell', 'env']:
        contract._require(contract._yaml_key_count(schema_step, 8, key) == 0, 'schema validation must run without overrides')
    contract._require(contract._yaml_key_count(schema_step, 8, 'run') == 1, 'one schema run owner required')
    schema_lines = contract._indented_block_lines(schema_step, '        run: |', 10)
    commands = tuple(line[10:] if line.startswith(' ' * 10) else line for line in schema_lines)
    contract._require(commands == (
        'python3 scripts/coverage_stability.py validate --kind baseline --input coverage/baseline.json',
        'python3 scripts/coverage_stability.py validate --kind measurement-exceptions --input coverage/measurement-exceptions.json',
    ), 'both exact schema validation commands are required')
    update_header = '      - name: Validate baseline update policy'
    update_lines = contract._indented_block_lines(baseline_job, update_header, 8)
    update_step = update_header + '\n' + '\n'.join(update_lines).rstrip() + '\n'

    measured_job = _job(manual_workflow, 'coverage')
    contract._require(contract._yaml_key_count(measured_job, 4, 'defaults') == 0, 'manual job shell defaults forbidden')
    measured_header = '      - name: Run clean coverage suite and ratchet\n'
    contract._require(measured_job.count(measured_header) == 1, 'one measurement step required')
    # Root Cargo environment manual workflow также проверяется: projection
    # использует main root, поэтому их различие нельзя молча скрывать.
    manual_environment = contract._indented_block_lines(manual_workflow, 'env:', 2)
    entries = tuple(line.strip() for line in manual_environment if line.strip() and not line.strip().startswith('#'))
    contract._require(entries == ('FASTIPLAYER_TEST_SCOPE: hosted', 'CARGO_INCREMENTAL: "0"', 'CARGO_TERM_COLOR: always'), 'manual Cargo environment changed')
    combined_job = measured_job.replace(measured_header, update_step + measured_header, 1)
    document = main_workflow.split('jobs:\n', 1)[0] + 'jobs:\n  coverage:\n' + combined_job
    contract.validate_coverage_workflow_contract(document)
    return document
