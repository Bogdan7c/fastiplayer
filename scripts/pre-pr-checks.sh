#!/usr/bin/env bash
# Локальная совместимая точка входа полного blocking CI набора.

# Строгий режим передаёт любую ошибку единого runner-а вызывающему shell.
set -Eeuo pipefail

# Каталог wrapper-а вычисляется независимо от текущего рабочего каталога.
script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"

# readonly не позволяет перенаправить wrapper на другой набор команд.
readonly SCRIPT_DIRECTORY="${script_directory}"

# Единый runner владеет командами, порядком и точной feature matrix.
"${SCRIPT_DIRECTORY}/ci-checks.sh" all
