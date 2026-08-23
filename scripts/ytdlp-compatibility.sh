#!/usr/bin/env bash
# Development-only runner проверяет системный yt-dlp через production boundaries Rustiplayer.

# Строгий shell mode запрещает продолжать работу после необработанной ошибки.
set -Eeuo pipefail

# Нулевой exit code означает доказанную совместимость.
readonly SUCCESS_EXIT_CODE=0
# Ненулевой exit code означает отсутствие executable либо нарушенный compatibility contract.
readonly FAILURE_EXIT_CODE=1

# Каталог скрипта вычисляется независимо от current working directory пользователя.
script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
# Корень репозитория является единственным working directory Cargo invocation.
repo_root="$(cd -- "${script_directory}/.." >/dev/null 2>&1 && pwd)"
# Immutable path исключает случайный запуск Cargo из соседнего workspace.
readonly REPO_ROOT="${repo_root}"
# Временный каталог создаётся только перед Cargo run и принадлежит cleanup trap.
compatibility_directory=""

# Cleanup удаляет только exact каталог hermetic executable shim-а.
cleanup() {
    # Пустое значение означает, что runner завершился до создания temporary directory.
    if [[ -n "${compatibility_directory}" && -d "${compatibility_directory}" ]]; then
        # Quoted exact path запрещает glob expansion и удаление соседних файлов.
        rm -rf -- "${compatibility_directory}"
    fi
}

# Trap выполняет cleanup при success, parser failure и interrupted Cargo run.
trap cleanup EXIT

# Help описывает development-only scope и точный outcome contract.
print_help() {
    # Quoted heredoc запрещает случайную shell interpolation документации.
    cat <<'EOF'
Usage: scripts/ytdlp-compatibility.sh

Checks the system yt-dlp executable through Rustiplayer's real candidate and
topology production APIs using a deterministic loopback HTTP fixture.

The check is development-only. A temporary shim adds --ignore-config and
--no-plugin-dirs only to the check, isolating upstream compatibility from the
user environment. Production invocation and plugin loading are unchanged.

Outcome contract:
  executable and both boundaries compatible   PASSED (exit 0)
  executable missing or contract violated      FAILED (exit non-zero)

Options:
  --help  Show this help.
EOF
}

# Создаёт временный command `yt-dlp`, который изолирует только development check.
create_hermetic_yt_dlp_shim() {
    # Mktemp атомарно создаёт owner-only directory вне repository tree.
    compatibility_directory="$(mktemp -d -t rustiplayer-ytdlp-compatibility.XXXXXX)"
    # Exact shim path должен называться `yt-dlp`, потому что production boundary ищет это имя в PATH.
    local shim_path="${compatibility_directory}/yt-dlp"
    # Quoted heredoc сохраняет runtime environment reference без преждевременной interpolation.
    cat >"${shim_path}" <<'EOF'
#!/usr/bin/env bash
# Strict mode запрещает продолжение после отсутствующего executable либо failed exec setup.
set -Eeuo pipefail
# System executable передаётся owner runner-ом как exact absolute diagnostic path.
readonly SYSTEM_YT_DLP_EXECUTABLE="${RUSTIPLAYER_SYSTEM_YT_DLP_EXECUTABLE:?missing system yt-dlp executable}"
# Exec сохраняет production arguments, добавляя только development isolation prefix.
exec "${SYSTEM_YT_DLP_EXECUTABLE}" --ignore-config --no-plugin-dirs "$@"
EOF
    # Executable bit делает shim первым exact `yt-dlp` command в Cargo child PATH.
    chmod +x "${shim_path}"
}

# Единый error formatter сохраняет машинно-различимый FAILED marker.
print_failed() {
    # Причина не содержит пользовательский URL либо process stdout/stderr.
    local reason="$1"
    # Diagnostic отправляется в stderr, как и вывод Cargo test.
    printf 'FAILED: system yt-dlp compatibility; reason=%s\n' "${reason}" >&2
}

# Parser намеренно принимает только help: у проверки нет скрытых режимов или version allowlist.
parse_arguments() {
    # Каждый argument обрабатывается явно, чтобы typo не запустил другой сценарий.
    while (($# > 0)); do
        # Exact option matching сохраняет маленький публичный CLI скрипта.
        case "$1" in
            # Help ничего не проверяет и завершается успешно.
            --help)
                # Документация печатается в stdout.
                print_help
                # Явный success не может быть ошибочно принят за compatibility PASS marker.
                exit "${SUCCESS_EXIT_CODE}"
                ;;
            # Любой неизвестный argument является ошибкой пользователя.
            *)
                # Неизвестный token безопасно печатается как shell data.
                print_failed "unknown argument '$1'"
                # Runner не запускает Cargo после parser failure.
                exit "${FAILURE_EXIT_CODE}"
                ;;
        esac
    done
}

# Main связывает executable discovery, version evidence и единственный integration test.
main() {
    # Аргументы валидируются до любых внешних процессов.
    parse_arguments "$@"

    # Production ищет exact command `yt-dlp` через PATH, поэтому runner проверяет тот же command.
    local yt_dlp_executable
    # Отсутствующий системный executable означает невозможность доказать совместимость.
    if ! yt_dlp_executable="$(command -v yt-dlp)"; then
        # Причина отличается от schema/process failure внутри Rust test.
        print_failed "yt-dlp executable was not found in PATH"
        # Явно выбранная проверка не может завершиться ложным успехом.
        exit "${FAILURE_EXIT_CODE}"
    fi

    # Version используется только как diagnostic evidence и никогда не сравнивается с allowlist.
    local detected_version
    # Даже diagnostic version command обязан завершиться успешно до compatibility run.
    if ! detected_version="$("${yt_dlp_executable}" --version)"; then
        # Неработающий executable не передаётся Cargo test-у.
        print_failed "yt-dlp --version failed"
        # Failure остаётся локальным development runner-у и не меняет production code.
        exit "${FAILURE_EXIT_CODE}"
    fi

    # Empty либо multiline version output считается сломанным diagnostic command contract.
    detected_version="${detected_version%%$'\n'*}"
    # Empty first line не даёт полезного provenance для failed compatibility report.
    if [[ -z "${detected_version}" ]]; then
        # Отдельная причина не утверждает несовместимость JSON schema.
        print_failed "yt-dlp --version returned an empty first line"
        # Cargo process boundary ещё не запускался.
        exit "${FAILURE_EXIT_CODE}"
    fi

    # RUN marker фиксирует exact executable и наблюдаемую, но не разрешаемую policy-ей версию.
    printf 'RUN: system yt-dlp compatibility; executable=%s; version=%s\n' \
        "${yt_dlp_executable}" "${detected_version}" >&2
    # Cargo всегда запускается из workspace root с locked dependency graph.
    cd "${REPO_ROOT}"
    # Temporary shim изолирует binary contract от user config/plugins только в development check.
    create_hermetic_yt_dlp_shim

    # Ignored integration test является единственным владельцем локального fixture и assertions.
    if ! env \
        "PATH=${compatibility_directory}:${PATH}" \
        "RUSTIPLAYER_SYSTEM_YT_DLP_EXECUTABLE=${yt_dlp_executable}" \
        cargo +1.96.0 test \
        -p service-ytdlp \
        --locked \
        --test system_ytdlp_compatibility \
        system_ytdlp_reaches_candidate_and_topology_boundaries \
        -- \
        --ignored \
        --exact \
        --nocapture; then
        # Shell boundary не подменяет typed Rust error своим парсингом output.
        print_failed "candidate or topology production boundary rejected the executable"
        # Ненулевой status пригоден для development gate и CI job.
        exit "${FAILURE_EXIT_CODE}"
    fi

    # PASSED печатается только после реального system executable и обоих production APIs.
    printf 'PASSED: system yt-dlp compatibility; executable=%s; version=%s\n' \
        "${yt_dlp_executable}" "${detected_version}" >&2
}

# Единственная top-level операция передаёт исходные CLI arguments без преобразования.
main "$@"
