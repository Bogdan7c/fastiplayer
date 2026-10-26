#!/usr/bin/env python3
"""Последовательная возобновляемая серия; protocol hash фиксируется до первого attempt.

Протокол v2: quiet-пары идут до 3/6/9 повторов с правилом остановки, diagnostic —
``diagnostic_repetitions`` повторов (по умолчанию 1) только для quality-gate и
описательного observer-control. Сцена может ограничить список плееров полем
``players`` (например, профиль, который эталон на этой машине не воспроизводит).

Протокол v4 (сессия 11 gles2-renderer, T410): план проверяет ``plan_v4.validate_plan``
до первой попытки; перед каждой попыткой проверяются питание и блокировка экрана
(``conditions.py``), рост ``GPU HANG`` останавливает серию кодом выхода 3, непригодная
среда — кодом 4 (код 2 занят quality-gate-ом); расписание quiet задаёт ``quiet_looks``
(``ladder.py``), потери при ``losses = annotate_all`` серию не останавливают.

Сессия 10 gles2-renderer: игроков два или три, у каждого может быть ``kind``; ``subject``
плана — Fastiplayer, которого судим, остальные — эталоны. Сцена останавливается на
3/6/9, когда терминален вердикт КАЖДОЙ пары «субъект × эталон» (см. ``players.py``).
"""

import argparse
import json
from pathlib import Path
import subprocess
import sys

from analyze import analyze
import conditions
from host import PROTOCOL_VERSION, sha256, write_json
import ladder
import plan_v4
from players import repetition_order



def run_attempt(root, tooling, plan, scenario, player, mode, repetition):
    """Запускает одну попытку; уже завершённая попытка не перезапускается.

    Возвращает код завершения collector-а (0 — валидное наблюдение) или 0 для
    уже сохранённой попытки. Прерванная попытка без ``result.json`` требует ручного
    разбора: заменять её молча нельзя.
    """
    label = f'{scenario["scenario"]}-{mode}-{repetition:02}-{player}'
    spec = plan_v4.attempt_spec(plan, scenario, player, mode, repetition)
    spec_path = root / 'specs' / f'{label}.json'
    write_json(spec_path, spec)
    attempt = root / 'attempts' / label
    if attempt.exists():
        if not (attempt / 'result.json').exists():
            raise ValueError(f'interrupted attempt requires review: {label}')
        return 0
    # Среда обязана годиться ДО запуска: заблокированный экран или батарея делают окно бессмысленным.
    conditions.require_runnable()
    with (root / 'runner.log').open('a') as log:
        completed = subprocess.run([sys.executable, str(tooling / 'collect.py'),
                                    str(spec_path), str(attempt)], stdout=log, stderr=subprocess.STDOUT)
    print(label, 'exit', completed.returncode, flush=True)
    raise_if_gpu_hang(attempt, label)
    return completed.returncode


def raise_if_gpu_hang(attempt, label):
    """Рост ``GPU HANG`` за попытку останавливает серию отдельным кодом (3), а не общим отказом."""
    result_path = attempt / 'result.json'
    if not result_path.exists():
        return
    if json.loads(result_path.read_text()).get('gpu_hang', {}).get('grew'):
        raise conditions.SeriesStop(conditions.EXIT_GPU_HANG,
                                    f'GPU HANG appeared in the kernel journal during {label}: '
                                    'series stopped, the GPU/driver needs review before continuing')


def ordered(values, repetition):
    """AB/BA: на чётных повторах порядок разворачивается, чтобы дрейф не копился у одного."""
    return values if repetition % 2 else tuple(reversed(values))


# Вердикты, после которых пара «субъект × эталон» больше не нуждается в повторах.
TERMINAL_VERDICTS = {'WIN', 'WORSE', 'NOT TESTED'}


def cohort_settled(cohort):
    """Когорта решена, когда терминален вердикт каждой её пары (у когорты без эталона пар нет)."""
    return all(pair['resource_verdict']['status'] in TERMINAL_VERDICTS for pair in cohort['pairs'].values())


def run_series(plan_path, root):
    plan_path, root = plan_path.resolve(), root.resolve()
    root.mkdir(parents=True, exist_ok=True)
    lock = root / 'plan.json'
    plan = json.loads(plan_path.read_text())
    if plan.get('protocol_version') != PROTOCOL_VERSION:
        raise ValueError(f'plan must declare protocol_version {PROTOCOL_VERSION}')
    # Порядок плееров задаёт план; два — AB/BA, три — латинский квадрат (players.py).
    all_players = plan_v4.validate_plan(plan)
    if lock.exists() and json.loads(lock.read_text()) != plan:
        raise ValueError('plan changed: use a new series directory')
    if not lock.exists():
        write_json(lock, plan)
    tooling = Path(__file__).resolve().parent
    source_hashes = {str(path.relative_to(tooling.parent)): sha256(path)
                     for path in sorted([*tooling.glob('*.py'), tooling / 'window_owner.c',
                                         tooling.parent / 't480s/collect.py', tooling.parent / 't480s/fullscreen.py'])}
    provenance = {'plan_sha256': sha256(lock), 'tool_sha256': source_hashes}
    tool_lock = root / 'tool-provenance.json'
    if tool_lock.exists() and json.loads(tool_lock.read_text()) != provenance:
        raise ValueError('collector changed: use a new series directory')
    write_json(tool_lock, provenance)
    (root / 'attempts').mkdir(exist_ok=True)
    (root / 'specs').mkdir(exist_ok=True)
    diagnostic_repetitions = int(plan.get('diagnostic_repetitions', 1))
    try:
        return run_repetitions(root, tooling, plan, all_players, diagnostic_repetitions)
    except conditions.SeriesStop as stop:
        # Стоп-кран среды: сводку по уже снятому сохраняем, причину говорим явно.
        write_json(root / 'summary.json', analyze(root))
        print(f'SERIES STOPPED (exit {stop.exit_code}): {stop}', flush=True)
        return stop.exit_code


def run_repetitions(root, tooling, plan, all_players, diagnostic_repetitions):
    """Цикл повторов серии: порядок игроков, расписание quiet (``quiet_looks``), остановки."""
    looks_by_scenario = {s['scenario']: ladder.quiet_looks(plan, s) for s in plan['scenarios']}
    # ``quiet_looks: []`` — quiet этой сцены не запускается вовсе.
    stopped = {(name, 'quiet') for name, looks in looks_by_scenario.items() if not looks}
    last_repetition = max([diagnostic_repetitions, *map(ladder.last_quiet_repetition, looks_by_scenario.values())])
    for repetition in range(1, last_repetition + 1):
        for scenario in plan['scenarios']:
            players = tuple(p for p in all_players if p in scenario.get('players', all_players))
            modes = ('quiet', 'diagnostic') if repetition <= diagnostic_repetitions else ('quiet',)
            for mode in ordered(modes, repetition):
                if (scenario['scenario'], mode) in stopped:
                    continue
                for player in repetition_order(players, repetition):
                    if run_attempt(root, tooling, plan, scenario, player, mode, repetition):
                        # Не удаляем/не заменяем неудачный запуск и не тратим полный
                        # corpus на уже доказанно сломанный путь измерения.
                        write_json(root / 'summary.json', analyze(root))
                        return 1
        summary = analyze(root)
        write_json(root / 'summary.json', summary)
        if repetition == diagnostic_repetitions:
            # ``annotate_all`` не даёт статуса FAIL (потери только аннотируются, см. analyze).
            failed = [name for name, gate in summary['quality_gates'].items() if gate['status'] == 'FAIL']
            if failed:
                # Решение владельца: «WIN по CPU с потерей кадров» не принимается —
                # серия останавливается на диагностике, CPU-таблицу не добираем.
                print('quality gate FAILED:', ', '.join(failed), flush=True)
                return 2
        stopped |= scenarios_done_with_quiet(plan, looks_by_scenario, summary, repetition, stopped)
        if repetition >= diagnostic_repetitions and all((s['scenario'], 'quiet') in stopped for s in plan['scenarios']):
            return 0
    return 0


def scenarios_done_with_quiet(plan, looks_by_scenario, summary, repetition, stopped):
    """Сцены, чей quiet закончился на этом повторе: пары решены или достигнут последний look.

    Последняя точка расписания закрывает сцену безусловно: ``[3]`` — ровно 3 пары без
    продления, даже если вердикт INCONCLUSIVE.
    """
    done = set()
    for scenario in plan['scenarios']:
        name, looks = scenario['scenario'], looks_by_scenario[scenario['scenario']]
        if (name, 'quiet') in stopped or repetition not in looks:
            continue
        cohorts = [c for c in summary['cohorts'] if c['scenario'] == name and c['mode'] == 'quiet']
        if repetition == looks[-1] or (cohorts and all(cohort_settled(cohort) for cohort in cohorts)):
            done.add((name, 'quiet'))
    return done


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('plan', type=Path)
    parser.add_argument('root', type=Path)
    args = parser.parse_args()
    return run_series(args.plan, args.root)


if __name__ == '__main__':
    raise SystemExit(main())
