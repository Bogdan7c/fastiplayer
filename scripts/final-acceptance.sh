#!/usr/bin/env bash
# Единая автоматизированная S42 acceptance-точка без manual URL/fixture side effects.

# Строгий режим немедленно передаёт failure любого существующего owner-а.
set -Eeuo pipefail

# Каталог скрипта вычисляется независимо от текущего рабочего каталога.
script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"

# readonly запрещает случайно подменить владельцев CI и coverage проверок.
readonly SCRIPT_DIRECTORY="${script_directory}"

# Функция печатает точный контракт S42 launcher-а.
print_help() {
    # Heredoc не выполняет shell-подстановки и остаётся читаемым в terminal.
    cat <<'EOF'
Usage: scripts/final-acceptance.sh

Runs:
  scripts/ci-checks.sh all
  scripts/coverage.sh check

Manual opt-in acceptance намеренно не запускается без явно переданных
пользователем URL/fixtures и после automated gate остаётся NOT RUN.
EOF
}

# Главная функция запрещает частичный или неявно сетевой режим.
main() {
    # Help является read-only запросом и не требует build tools.
    if (($# == 1)) && [[ "$1" == "--help" || "$1" == "-h" ]]; then
        # Показываем стабильный CLI-контракт.
        print_help
        # Успешно завершаем read-only вызов.
        return 0
    fi
    # Любой аргумент мог бы ошибочно выглядеть как разрешение на manual URL run.
    if (($# != 0)); then
        # Диагностика объясняет, почему launcher не принимает URL/fixtures.
        printf 'Ошибка: S42 automated launcher не принимает аргументы.\n' >&2
        # Справка показывает отдельный manual opt-in контракт.
        print_help >&2
        # Код 2 отличает CLI misuse от failed acceptance check.
        return 2
    fi
    # Полный CI owner запускает locked compile/test/policy/patch matrix.
    "${SCRIPT_DIRECTORY}/ci-checks.sh" all
    # Coverage owner отдельно запускает clean hermetic suite и blocking ratchet.
    "${SCRIPT_DIRECTORY}/coverage.sh" check
    # Manual acceptance не подделывается автоматическим PASS без user inputs.
    printf '\nS42 automated acceptance: PASS\n'
    # Статус manual части формулируется однозначно для release report.
    printf 'S42 manual opt-in acceptance: NOT RUN (explicit user URL/fixtures required)\n'
}

# Единственная process boundary передаёт исходный argv без преобразований.
main "$@"
