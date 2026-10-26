"""Сборка спецификации попытки и проверка плана протокола v4 ДО первой попытки.

Новое в v4 (README, «Measurement protocol, version 4»):
- запись игрока может нести ``decode`` (переопределяет ``common.decode`` для этого игрока);
- сцена может нести ``player_overrides: {<игрок>: {args?, expect?, decode?}}`` — лучший режим
  соперника разный по сценам, поэтому переопределение действует на одну сцену;
- ``quiet_looks`` / ``losses`` (см. ``ladder.py``) и ``intel_gpu_top`` (bool в ``common``).

Порядок слоёв спецификации (позднее побеждает): ``common`` < поля сцены < запись игрока <
``player_overrides`` сцены. Ошибка плана должна всплыть до запуска, а не на третьем повторе
в виде невалидной попытки после целого окна замера.
"""

import collect
import ladder
from players import subject_name, validate_players

# Поля, которые сцена вправе переопределить для одного игрока.
OVERRIDE_FIELDS = ('args', 'expect', 'decode')
# Ключи сцены, которые не попадают в спецификацию попытки как есть.
SCENARIO_ONLY_KEYS = ('players', 'player_overrides')


def attempt_spec(plan, scenario, player, mode, repetition):
    """Спецификация одной попытки (то, что читает ``collect.py``)."""
    scenario_fields = {key: value for key, value in scenario.items() if key not in SCENARIO_ONLY_KEYS}
    overrides = scenario.get('player_overrides', {}).get(player, {})
    spec = {**plan['common'], **scenario_fields, **plan['players'][player], **overrides,
            'player': player, 'mode': mode, 'repetition': repetition}
    if 'subject' in plan:
        # Субъект нужен analyze.py на уровне попытки; старые планы без ключа не меняются.
        spec['subject'] = subject_name(plan)
    return spec


def _validate_overrides(plan, scenario):
    overrides = scenario.get('player_overrides', {})
    if not isinstance(overrides, dict):
        raise ValueError(f'scenario {scenario["scenario"]!r}: player_overrides must be an object')
    for name, fields in overrides.items():
        if name not in plan['players']:
            raise ValueError(f'scenario {scenario["scenario"]!r}: player_overrides names unknown player {name!r}')
        unknown = sorted(set(fields) - set(OVERRIDE_FIELDS))
        if unknown:
            raise ValueError(f'scenario {scenario["scenario"]!r}: player {name!r} override has unknown '
                             f'fields {unknown}; allowed {list(OVERRIDE_FIELDS)}')


def validate_plan(plan):
    """Проверяет план целиком; возвращает имена игроков (как ``validate_players``)."""
    names = validate_players(plan)
    common = plan.get('common', {})
    if 'intel_gpu_top' in common and not isinstance(common['intel_gpu_top'], bool):
        raise ValueError('common.intel_gpu_top must be true or false')
    for scenario in plan['scenarios']:
        _validate_overrides(plan, scenario)
        ladder.quiet_looks(plan, scenario)
        ladder.loss_policy(plan, scenario)
        for player in scenario.get('players', names):
            if player not in names:
                raise ValueError(f'scenario {scenario["scenario"]!r} lists unknown player {player!r}')
            # Все проверки collect.py над ЭФФЕКТИВНОЙ спецификацией: декод по виду игрока,
            # args/expect эталонов, согласованность VLC decode ↔ --avcodec-hw.
            spec = attempt_spec(plan, scenario, player, 'quiet', 1)
            collect.decode_path(spec)
            collect.player_extra_args(spec)
            collect.mpv_expectations(spec)
    return names
