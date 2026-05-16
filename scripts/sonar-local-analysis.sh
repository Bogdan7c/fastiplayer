#!/usr/bin/env bash
# Скрипт запускает локальный анализ Rust workspace через Clippy и SonarScanner.

# Строгий режим останавливает выполнение при ошибках, unset-переменных и сбоях в pipeline.
set -Eeuo pipefail

# Константа с кодом успешного завершения делает проверки читаемыми.
readonly SUCCESS_EXIT_CODE=0

# Путь к отчету можно переопределить, но по умолчанию он совпадает с sonar-project.properties.
readonly DEFAULT_CLIPPY_REPORT_PATH="target/sonar/clippy-report.json"

# Локальный SonarQube по умолчанию слушает только localhost.
readonly DEFAULT_SONAR_HOST_URL="http://127.0.0.1:9000"

# Имя бинарника можно переопределить, если scanner установлен не в PATH.
readonly DEFAULT_SONAR_SCANNER_BIN="sonar-scanner"

# Каталог отчета вычисляется из итогового пути, чтобы не держать вторую настройку вручную.
clippy_report_path="${CLIPPY_REPORT_PATH:-${DEFAULT_CLIPPY_REPORT_PATH}}"

# URL сервера берется из окружения или из локального дефолта.
sonar_host_url="${SONAR_HOST_URL:-${DEFAULT_SONAR_HOST_URL}}"

# Бинарник SonarScanner берется из окружения или из PATH.
sonar_scanner_bin="${SONAR_SCANNER_BIN:-${DEFAULT_SONAR_SCANNER_BIN}}"

# Функция печатает ошибку в stderr с единым префиксом.
print_error() {
    # Сообщение передается первым аргументом, чтобы caller формулировал причину явно.
    local error_message="$1"

    # stderr нужен, чтобы ошибка не смешивалась с JSON-отчетом Clippy.
    printf 'Ошибка: %s\n' "${error_message}" >&2
}

# Функция проверяет, что обязательная переменная окружения задана.
require_sonar_token() {
    # Пустой токен невалиден и не должен приводить к анонимному анализу.
    if [[ -z "${SONAR_TOKEN:-}" ]]; then
        # Пользователь должен создать токен вручную в UI и передать его только через окружение.
        print_error "задайте SONAR_TOKEN, например: SONAR_TOKEN=<token> $0"

        # Завершаем работу до любых сетевых запросов к SonarQube.
        exit 1
    fi
}

# Функция проверяет, что нужная команда доступна до начала долгого анализа.
require_command() {
    # Имя команды передается первым аргументом, чтобы функция была переиспользуемой.
    local required_command="$1"

    # command -v надежно проверяет PATH без запуска команды.
    if ! command -v "${required_command}" >/dev/null 2>&1; then
        # Ошибка содержит конкретное имя отсутствующей команды.
        print_error "команда '${required_command}' не найдена в PATH"

        # Продолжать нельзя, потому что следующий шаг точно завершится неуспешно.
        exit 1
    fi
}

# Функция создает каталог для отчета Clippy.
prepare_report_directory() {
    # dirname отделяет каталог от имени файла отчета.
    local clippy_report_directory

    # Команда dirname безопасно работает и для вложенных путей вида target/sonar/report.json.
    clippy_report_directory="$(dirname "${clippy_report_path}")"

    # mkdir -p не падает, если каталог уже существует.
    mkdir -p "${clippy_report_directory}"
}

# Функция запускает Clippy и сохраняет JSON-события Cargo в файл для импорта SonarQube.
run_clippy_report() {
    # Информационное сообщение идет в stderr, чтобы stdout оставался свободным для машинных данных.
    printf 'Готовлю Clippy JSON: %s\n' "${clippy_report_path}" >&2

    # Cargo пишет JSON-события в stdout, поэтому перенаправляем stdout прямо в отчет.
    cargo clippy --workspace --all-targets --message-format=json >"${clippy_report_path}"
}

# Функция запускает SonarScanner с токеном и URL из окружения.
run_sonar_scanner() {
    # Информационное сообщение не содержит токен и показывает только адрес сервера.
    printf 'Запускаю SonarScanner для %s\n' "${sonar_host_url}" >&2

    # export передает настройки scanner без записи секрета в репозиторий.
    export SONAR_HOST_URL="${sonar_host_url}"

    # export передает токен через окружение, чтобы не класть его в sonar-project.properties.
    export SONAR_TOKEN

    # Путь отчета передаем явно, чтобы переопределение CLIPPY_REPORT_PATH не расходилось с анализом.
    "${sonar_scanner_bin}" -Dsonar.rust.clippyReport.reportPaths="${clippy_report_path}"
}

# Главная функция держит порядок шагов анализа в одном месте.
main() {
    # Проверяем токен до любых затратных локальных операций.
    require_sonar_token

    # Проверяем Cargo, потому что Clippy запускается через cargo clippy.
    require_command "cargo"

    # Проверяем SonarScanner до запуска Clippy, чтобы быстрее показать ошибку установки.
    require_command "${sonar_scanner_bin}"

    # Создаем каталог отчета перед перенаправлением stdout Clippy.
    prepare_report_directory

    # Генерируем внешний отчет Clippy для SonarQube.
    run_clippy_report

    # Загружаем результаты анализа в локальный SonarQube.
    run_sonar_scanner
}

# Запуск main сохраняет возможность позже тестировать функции отдельно.
main "$@"

# Явное успешное завершение делает намерение скрипта очевидным.
exit "${SUCCESS_EXIT_CODE}"
