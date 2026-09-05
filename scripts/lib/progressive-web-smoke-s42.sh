#!/usr/bin/env bash
# Связный owner S42 safe-case matrix, provenance, redaction и manual report contract.

# Этот файл разрешено только source-ить из fail-closed runner-а.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    printf 'Ошибка: S42 manual library не является самостоятельной командой\n' >&2
    exit 2
fi

# Повторный source мог бы переинициализировать arrays после parsing.
if [[ "${FASTIPLAYER_S42_LIBRARY_LOADED:-false}" == "true" ]]; then
    return 0
fi
# Guard становится readonly после первого успешного source.
readonly FASTIPLAYER_S42_LIBRARY_LOADED="true"

# S42 принимает manual evidence только от exact release утверждённого profile.
readonly EXPECTED_YTDLP_VERSION="2026.07.04"
# Stable ID связывает report с machine-readable S00/S41 artifacts.
readonly S42_PROFILE_ID="yt-dlp-2026.07.04-serializable-v1"
# Pinned upstream commit отличает profile provenance от workspace commit.
readonly S42_PROFILE_SOURCE_COMMIT="fdec00e0bf530dc6c3cc7b1dd780e95d9ae460e9"
# Полный safe-case набор: отсутствие любого ID остаётся NOT RUN.
readonly -a REQUIRED_S42_CASE_IDS=(
    "playlist-m3u8"
    "playlist-xspf"
    "playlist-cue"
    "compound-multi-video"
    "public-single"
    "public-playlist"
    "public-channel"
    "public-search"
    "protected-system-cookie"
    "progressive-http-iso-bmff"
    "progressive-http-matroska-webm"
    "progressive-http-proven-audio"
    "hls-vod-ts"
    "hls-vod-fmp4"
    "hls-live-dvr"
    "dash-vod-fmp4"
    "dash-vod-webm"
    "dash-live-dvr"
    "ism-mss-base-h264-aac-fmp4"
    "ftp-ftps-progressive"
    "hds-f4m-f4f"
    "layout-muxed"
    "layout-separate"
    "layout-video-only"
    "layout-audio-only"
    "quality-preference-switch"
    "pre-barrier-import"
    "pre-barrier-open"
    "pre-barrier-switch"
)

# Safe IDs появляются только из allowlist либо legacy URL mapping.
declare -a scenario_case_ids=()
# Input kind отделяет URL от fixture без raw identity в report.
declare -a scenario_input_kinds=()
# Raw inputs живут только в shell memory и temporary logs.
declare -a scenario_inputs=()
# Missing IDs вычисляются только из safe allowlist.
declare -a missing_s42_case_ids=()
# Exact yt-dlp version заполняется после real-run preflight.
observed_ytdlp_version=""
# Executable digest является non-secret provenance.
observed_ytdlp_sha256=""
# Workspace commit связывает review с source snapshot.
workspace_commit=""
# Workspace state честно отделяет reproducible clean HEAD от локального worktree.
workspace_tree_state=""

# Проверяет принадлежность ID закрытой S42 matrix.
is_required_s42_case_id() {
    # Safe ID передаётся первым аргументом.
    local candidate_case_id="$1"
    # Exact loop не допускает prefix aliases.
    local required_case_id
    # Каждый known ID сравнивается byte-for-byte.
    for required_case_id in "${REQUIRED_S42_CASE_IDS[@]}"; do
        # Exact match завершает validation.
        if [[ "${candidate_case_id}" == "${required_case_id}" ]]; then
            return 0
        fi
    done
    # Unknown ID не становится fake evidence.
    return 1
}

# Возвращает допустимый input kind exact case-а.
case_input_kind() {
    # Safe ID приходит из parser-а.
    local candidate_case_id="$1"
    # Playlist actions и failed import требуют local fixture.
    case "${candidate_case_id}" in
        playlist-m3u8 | playlist-xspf | playlist-cue | pre-barrier-import)
            printf '%s\n' "fixture"
            ;;
        *)
            printf '%s\n' "url"
            ;;
    esac
}

# Проверяет safe case ID до чтения raw input.
validate_case_id() {
    # Exact safe label передаётся первым аргументом.
    local candidate_case_id="$1"
    # Unknown labels запрещены.
    if ! is_required_s42_case_id "${candidate_case_id}"; then
        print_error "--case не входит в утверждённую S42 manual matrix"
        exit "${USAGE_EXIT_CODE}"
    fi
}

# URL validation допускает только production-supported S42 schemes.
validate_explicit_url() {
    # Exact URL никогда не печатается.
    local candidate_url="$1"
    # Control characters могли бы подделать report.
    if [[ "${candidate_url}" =~ [[:cntrl:]] ]]; then
        print_error "--url не должен содержать управляющие символы"
        exit "${USAGE_EXIT_CODE}"
    fi
    # RTMP и excluded schemes fail closed.
    if [[ ! "${candidate_url}" =~ ^(https?|ftps?)://[^/[:space:]]+(/[^[:space:]]*)?$ ]]; then
        print_error "--url должен быть explicit absolute HTTP/HTTPS/FTP/FTPS URL"
        exit "${USAGE_EXIT_CODE}"
    fi
}

# Fixture validation запрещает report-control characters.
validate_explicit_fixture() {
    # Exact path передаётся первым аргументом.
    local candidate_fixture="$1"
    # Empty path не является selection.
    if [[ -z "${candidate_fixture}" ]]; then
        print_error "--fixture не должен быть пустым"
        exit "${USAGE_EXIT_CODE}"
    fi
    # Control characters запрещены.
    if [[ "${candidate_fixture}" =~ [[:cntrl:]] ]]; then
        print_error "--fixture не должен содержать управляющие символы"
        exit "${USAGE_EXIT_CODE}"
    fi
}

# Проверяет соответствие role и explicit input.
validate_case_input() {
    # Safe case ID передаётся первым аргументом.
    local candidate_case_id="$1"
    # URL/fixture kind передаётся вторым.
    local candidate_input_kind="$2"
    # Raw input нужен только для scheme validation.
    local candidate_input="$3"
    # Named case имеет ровно один kind.
    local expected_input_kind
    # Pure helper возвращает expected kind.
    expected_input_kind="$(case_input_kind "${candidate_case_id}")"
    # Kind mismatch блокируется до arrays mutation.
    if [[ "${candidate_input_kind}" != "${expected_input_kind}" ]]; then
        print_error "--case получил неподходящий тип --url/--fixture"
        exit "${USAGE_EXIT_CODE}"
    fi
    # FTP row требует exact FTP/FTPS family.
    if [[ "${candidate_case_id}" == "ftp-ftps-progressive" && ! "${candidate_input}" =~ ^ftps?:// ]]; then
        print_error "ftp-ftps-progressive требует explicit FTP/FTPS URL"
        exit "${USAGE_EXIT_CODE}"
    fi
    # Остальные URL cases требуют HTTP/HTTPS.
    if [[ "${candidate_input_kind}" == "url" && "${candidate_case_id}" != "ftp-ftps-progressive" && ! "${candidate_input}" =~ ^https?:// ]]; then
        print_error "этот --case требует explicit HTTP/HTTPS URL"
        exit "${USAGE_EXIT_CODE}"
    fi
}

# Проверяет exact-once safe case selection.
ensure_unique_case_id() {
    # Новый ID передаётся первым аргументом.
    local candidate_case_id="$1"
    # Existing IDs не содержат media identity.
    local existing_case_id
    # Duplicate мог бы скрыть missing axis.
    for existing_case_id in "${scenario_case_ids[@]}"; do
        # Exact duplicate запрещён.
        if [[ "${candidate_case_id}" == "${existing_case_id}" ]]; then
            print_error "--case нельзя указывать повторно"
            exit "${USAGE_EXIT_CODE}"
        fi
    done
}

# Добавляет один validated scenario.
add_scenario() {
    # Safe case ID передаётся первым.
    local candidate_case_id="$1"
    # URL/fixture kind передаётся вторым.
    local candidate_input_kind="$2"
    # Exact input передаётся третьим.
    local candidate_input="$3"
    # Duplicate проверяется до mutation.
    ensure_unique_case_id "${candidate_case_id}"
    # Parallel arrays сохраняют one-to-one order.
    scenario_case_ids+=("${candidate_case_id}")
    # Kind является safe metadata.
    scenario_input_kinds+=("${candidate_input_kind}")
    # Raw input не сериализуется напрямую.
    scenario_inputs+=("${candidate_input}")
}

# Вычисляет отсутствующие S42 cases.
collect_missing_s42_case_ids() {
    # Повторный вызов начинает с пустого результата.
    missing_s42_case_ids=()
    # Required ID приходит из readonly allowlist.
    local required_case_id
    # Selected ID также safe.
    local selected_case_id
    # Каждый required ID проверяется exact.
    for required_case_id in "${REQUIRED_S42_CASE_IDS[@]}"; do
        # До match row отсутствует.
        local case_found="false"
        # Bounded linear scan прозрачен.
        for selected_case_id in "${scenario_case_ids[@]}"; do
            # Exact equality закрывает row.
            if [[ "${required_case_id}" == "${selected_case_id}" ]]; then
                case_found="true"
                break
            fi
        done
        # Missing safe label публикуется как NOT RUN.
        if [[ "${case_found}" == "false" ]]; then
            missing_s42_case_ids+=("${required_case_id}")
        fi
    done
}

# Реальный запуск требует readable explicit fixtures.
validate_real_fixture_inputs() {
    # Parallel offset связывает kind/input.
    local scenario_offset
    # Dry-run не требует owner-local state.
    if [[ "${dry_run}" == "true" ]]; then
        return
    fi
    # Каждый fixture проверяется без печати path.
    for scenario_offset in "${!scenario_case_ids[@]}"; do
        # URL не является filesystem input.
        if [[ "${scenario_input_kinds[scenario_offset]}" != "fixture" ]]; then
            continue
        fi
        # Missing/unreadable fixture блокирует evidence.
        if [[ ! -f "${scenario_inputs[scenario_offset]}" || ! -r "${scenario_inputs[scenario_offset]}" ]]; then
            print_error "explicit --fixture не найден либо недоступен для чтения"
            exit "${FAILURE_EXIT_CODE}"
        fi
        # Canonical path вычисляется до смены cwd и никогда не печатается.
        local canonical_fixture_path
        # stderr realpath подавляется, чтобы diagnostics не раскрыла raw fixture identity.
        if ! canonical_fixture_path="$(
            realpath --canonicalize-existing -- "${scenario_inputs[scenario_offset]}" 2>/dev/null
        )"; then
            print_error "explicit --fixture не удалось безопасно нормализовать"
            exit "${FAILURE_EXIT_CODE}"
        fi
        # Absolute canonical identity исключает повторную интерпретацию relative path приложением.
        if [[ "${canonical_fixture_path}" != /* ]]; then
            print_error "explicit --fixture не разрешился в absolute path"
            exit "${FAILURE_EXIT_CODE}"
        fi
        # Runtime и redactor получают ровно одну нормализованную identity.
        scenario_inputs[scenario_offset]="${canonical_fixture_path}"
    done
}

# Проверяет pinned yt-dlp и собирает provenance.
verify_ytdlp_provenance() {
    # PATH resolution совпадает с app inheritance.
    local ytdlp_executable
    # Path не публикуется.
    ytdlp_executable="$(command -v yt-dlp)"
    # Version probe изолирован от trusted config/plugins.
    if ! observed_ytdlp_version="$("${ytdlp_executable}" --ignore-config --no-plugin-dirs --version 2>/dev/null)"; then
        print_error "не удалось проверить exact system yt-dlp provenance"
        exit "${FAILURE_EXIT_CODE}"
    fi
    # Mismatch блокирует profile acceptance.
    if [[ "${observed_ytdlp_version}" != "${EXPECTED_YTDLP_VERSION}" ]]; then
        print_error "system yt-dlp version не совпадает с утверждённым 2026.07.04 profile"
        exit "${FAILURE_EXIT_CODE}"
    fi
    # SHA utility читает executable bytes.
    local sha256_output
    # Failure не создаёт неполный report.
    if ! sha256_output="$(sha256sum -- "${ytdlp_executable}")"; then
        print_error "не удалось вычислить SHA-256 system yt-dlp"
        exit "${FAILURE_EXIT_CODE}"
    fi
    # Первый field является digest.
    observed_ytdlp_sha256="${sha256_output%% *}"
    # Malformed digest запрещён.
    if [[ ! "${observed_ytdlp_sha256}" =~ ^[0-9a-fA-F]{64}$ ]]; then
        print_error "SHA-256 system yt-dlp имеет некорректный формат"
        exit "${FAILURE_EXIT_CODE}"
    fi
    # Workspace commit нужен для reproducibility.
    if ! workspace_commit="$(git -C "${REPO_ROOT}" rev-parse --verify HEAD 2>/dev/null)"; then
        print_error "не удалось определить workspace commit для manual report"
        exit "${FAILURE_EXIT_CODE}"
    fi
    # Object ID format остаётся bounded.
    if [[ ! "${workspace_commit}" =~ ^[0-9a-fA-F]{40,64}$ ]]; then
        print_error "workspace commit имеет некорректный формат"
        exit "${FAILURE_EXIT_CODE}"
    fi
    # Status payload нужен только для clean/dirty classification и не попадает в report.
    local workspace_status
    # Git failure не позволяет приписать binary неверному source state.
    if ! workspace_status="$(
        git -C "${REPO_ROOT}" status --porcelain=v1 --untracked-files=normal 2>/dev/null
    )"; then
        print_error "не удалось определить состояние workspace для manual report"
        exit "${FAILURE_EXIT_CODE}"
    fi
    # Пустой porcelain означает exact clean HEAD; любой path/change делает state dirty.
    if [[ -z "${workspace_status}" ]]; then
        workspace_tree_state="clean"
    else
        workspace_tree_state="dirty"
    fi
}

# Удаляет exact input, network endpoints и secret-bearing lines.
redact_runtime_log() {
    # Raw log path принадлежит temp owner-у.
    local raw_log_path="$1"
    # Exact URL/fixture нужен literal replacement.
    local exact_input="$2"
    # Kind включает filesystem-derived redaction только для fixture scenario.
    local input_kind="$3"
    # Basename закрывает diagnostics, которые сокращают canonical fixture до имени файла.
    local fixture_basename=""
    # Percent-encoded basename закрывает типичное URI/display преобразование пробелов.
    local encoded_fixture_basename=""
    # URL scenario не получает filesystem-derived aliases.
    if [[ "${input_kind}" == "fixture" ]]; then
        fixture_basename="${exact_input##*/}"
        encoded_fixture_basename="${fixture_basename// /%20}"
    fi
    # Environment сохраняет literal backslashes.
    env \
        "FASTIPLAYER_REDACT_INPUT=${exact_input}" \
        "FASTIPLAYER_REDACT_KIND=${input_kind}" \
        "FASTIPLAYER_REDACT_BASENAME=${fixture_basename}" \
        "FASTIPLAYER_REDACT_ENCODED_BASENAME=${encoded_fixture_basename}" \
        awk '
        BEGIN {
            exact_input = ENVIRON["FASTIPLAYER_REDACT_INPUT"]
            input_kind = ENVIRON["FASTIPLAYER_REDACT_KIND"]
            fixture_basename = ENVIRON["FASTIPLAYER_REDACT_BASENAME"]
            encoded_fixture_basename = ENVIRON["FASTIPLAYER_REDACT_ENCODED_BASENAME"]
        }
        function replace_exact(text, secret, position) {
            if (secret == "") {
                return text
            }
            while ((position = index(text, secret)) > 0) {
                text = substr(text, 1, position - 1) "<redacted-input>" substr(text, position + length(secret))
            }
            return text
        }
        {
            lower_line = tolower($0)
            if (lower_line ~ /authorization|cookie|set-cookie|header|request[_ -]?data|requested[_ -]?formats|extractor|payload|token|signature|password|secret|bearer/) {
                print "<redacted-secret-line>"
                next
            }
            sanitized_line = replace_exact($0, exact_input)
            gsub(/(https?|ftps?):\/\/[^[:space:]<>"]+/, "<redacted-url>", sanitized_line)
            if (input_kind == "fixture") {
                sanitized_line = replace_exact(sanitized_line, fixture_basename)
                sanitized_line = replace_exact(sanitized_line, encoded_fixture_basename)
                gsub(/file:\/\/[^[:space:]<>"]+/, "<redacted-fixture-uri>", sanitized_line)
                gsub(/\/[^[:space:]<>"]+/, "<redacted-fixture-path>", sanitized_line)
                gsub(/%2[fF][^[:space:]<>"]+/, "<redacted-fixture-path>", sanitized_line)
            }
            print sanitized_line
        }
    ' "${raw_log_path}"
}

# Печатает selected/missing inventory.
write_case_inventory() {
    # Offset связывает safe ID с kind.
    local scenario_offset
    # Selected scope проверяем человеком.
    printf '## Selected safe case IDs\n\n'
    # Raw identities отсутствуют.
    for scenario_offset in "${!scenario_case_ids[@]}"; do
        printf -- '- `%s` (%s; raw identity not retained)\n' \
            "${scenario_case_ids[scenario_offset]}" \
            "${scenario_input_kinds[scenario_offset]}"
    done
    # Missing section.
    printf '\n## Missing required S42 case IDs\n\n'
    # Complete selection всё равно не PASS.
    if ((${#missing_s42_case_ids[@]} == 0)); then
        printf '%s\n\n' 'None. Matrix still requires human review; this is not PASS.'
        return
    fi
    # Missing IDs safe/actionable.
    local missing_case_id
    # Печатаем exact allowlisted names.
    for missing_case_id in "${missing_s42_case_ids[@]}"; do
        printf -- '- `%s`\n' "${missing_case_id}"
    done
    # Section delimiter.
    printf '\n'
}

# Печатает no-op/exclusion inventory.
write_profile_exclusion_inventory() {
    # No fake provider tests.
    printf '## Checked-in no-op and ProfileExcluded evidence\n\n'
    # Aggregate RTMP остаётся identity-only.
    printf -- '- RTMP family: `ProfileExcluded`; no deterministic wire provider/fixture.\n'
    # ISM live не расширяет VOD.
    printf -- '- ISM live/DVR: `ProfileExcluded`; approved profile contains static VOD only.\n'
    # HDS live не имеет approved row.
    printf -- '- HDS live/DVR: `NoApprovedRow`; approved profile contains HDS VOD only.\n'
    # Private special state остаётся excluded.
    printf -- '- Special private/live providers: `NoApprovedRow`; no fake provider admission.\n'
    # Roadmap exclusions explicit.
    printf -- '- RTSP/RTP/MMS/private-live/DRM: explicit profile exclusions.\n\n'
}

# Печатает полный human checklist.
write_manual_checklist() {
    # Manual checks никогда не отмечаются shell-ом.
    printf '## Manual checklist\n\n'
    # Playlist formats.
    printf -- '- [ ] M3U8 import, full/selected export and re-import preserve expected identities.\n'
    printf -- '- [ ] XSPF import, full/selected export and re-import preserve compound metadata.\n'
    printf -- '- [ ] CUE import/export preserves exact track windows; unrepresentable export is typed.\n'
    # Compound/topology.
    printf -- '- [ ] `multi_video` renders one compound top-level entry, disclosure/header/part actions and exact part navigation.\n'
    printf -- '- [ ] public single URL opens one item through direct-media-first routing.\n'
    printf -- '- [ ] public playlist URL previews/commits the bounded collection without silent drops.\n'
    printf -- '- [ ] public channel URL preserves partial/unavailable/duplicate topology semantics.\n'
    printf -- '- [ ] public search URL preserves bounded order and explicit confirmation semantics.\n'
    # Auth/provider rows.
    printf -- '- [ ] system-cookie protected URL opens using trusted system yt-dlp config without app credential persistence.\n'
    printf -- '- [ ] progressive HTTP ISO-BMFF, Matroska/WebM and proven-audio rows play through the shared path.\n'
    printf -- '- [ ] HLS VOD TS, HLS VOD fMP4 and HLS live/DVR cases match their exact profile boundaries.\n'
    printf -- '- [ ] DASH VOD fMP4, DASH VOD WebM and DASH live/DVR cases match their exact profile boundaries.\n'
    printf -- '- [ ] ISM/MSS H.264+AAC static VOD, FTP/FTPS progressive and HDS/F4F VOD cases work.\n'
    # Layout/quality/lifecycle.
    printf -- '- [ ] muxed, separate A/V, video-only and audio-only cases each reach the expected active layout.\n'
    printf -- '- [ ] global preferred height and per-item runtime quality switch work while Playing and Paused.\n'
    printf -- '- [ ] VOD terminal end and live/DVR range, expiry, starvation and safe live-edge behavior are correct.\n'
    # Barrier semantics.
    printf -- '- [ ] failed pre-barrier import preserves queue/current playback.\n'
    printf -- '- [ ] failed pre-barrier open preserves current playback.\n'
    printf -- '- [ ] failed pre-barrier quality/component switch preserves current playback.\n'
    printf -- '- [ ] post-barrier failure is reported as terminal lifecycle, not recoverable rollback.\n'
    # Secret/cancel/shutdown.
    printf -- '- [ ] URL sidebar has no second URL input and shows only secret-safe source state.\n'
    printf -- '- [ ] acknowledged exact locator persists separately from transient headers/cookies/targets.\n'
    printf -- '- [ ] supersede/cancel/stale completion and normal shutdown publish no stale active source.\n'
    printf -- '- [ ] saved report contains no raw URL, fixture path, header, cookie, token or extractor payload.\n\n'
}

# Создаёт header только после complete provenance preflight.
write_report_header() {
    # Owner-only permissions применяются до write.
    umask 077
    # Missing cases => NOT RUN; complete selection => manual review.
    local matrix_status
    # Automatic PASS отсутствует.
    if ((${#missing_s42_case_ids[@]} == 0)); then
        matrix_status="MANUAL REVIEW REQUIRED"
    else
        matrix_status="NOT RUN"
    fi
    # Noclobber и redirection живут в одном subshell, поэтому create остаётся exclusive.
    if ! (
        # Shell использует O_EXCL-like create и не доверяет более раннему preflight check.
        set -o noclobber
        # Header полностью пишется через первый и единственный create report artifact-а.
        {
            printf '# S42 web-media manual acceptance report\n\n'
            printf 'Report lifecycle: runtime evidence pending\n'
            printf 'S42 matrix status: %s\n' "${matrix_status}"
            printf 'Selected case count: %s\n' "${#scenario_case_ids[@]}"
            printf 'Missing required case count: %s\n' "${#missing_s42_case_ids[@]}"
            printf 'Per-case timebox seconds: %s\n' "${duration_seconds}"
            printf 'Compatibility profile ID: `%s`\n' "${S42_PROFILE_ID}"
            printf 'Compatibility profile source commit: `%s`\n' "${S42_PROFILE_SOURCE_COMMIT}"
            printf 'Workspace HEAD commit: `%s`\n' "${workspace_commit}"
            printf 'Workspace state: `%s`\n' "${workspace_tree_state}"
            printf 'Fastiplayer binary origin: `%s`\n' "${selected_binary_origin}"
            printf 'Fastiplayer binary SHA-256: `%s`\n' "${selected_binary_sha256}"
            if [[ "${selected_binary_origin}" == "runner-built-from-current-worktree" ]]; then
                printf 'Fastiplayer source association: current worktree; dirty state is not reproducible from HEAD alone\n'
            else
                printf 'Fastiplayer source association: external prebuilt; workspace HEAD is not asserted as its source\n'
            fi
            printf 'System yt-dlp version: `%s`\n' "${observed_ytdlp_version}"
            printf 'System yt-dlp executable SHA-256: `%s`\n' "${observed_ytdlp_sha256}"
            printf 'System yt-dlp config/plugin/cookie lookup during app run: preserved\n'
            printf 'Raw URLs/fixtures/headers/cookies/extractor payloads: not retained\n'
            printf 'Hardware: owner-approved exact VAProfileH264Baseline -> H.264 Baseline 8-bit YUV420/NV12, capability intersection only; current manual rerun NOT RUN (owner has no compatible VA-API device)\n\n'
            write_case_inventory
            write_profile_exclusion_inventory
            write_manual_checklist
            printf '## Sanitized runtime evidence\n'
        } >"${report_path}"
    ) 2>/dev/null; then
        # Generic diagnostics не раскрывает выбранный report path.
        print_error "не удалось atomically создать новый --report; existing artifact не перезаписан"
        exit "${FAILURE_EXIT_CODE}"
    fi
}

# Завершает report единственным authoritative runner outcome.
write_final_report_outcome() {
    # Aggregate process status передаётся после выполнения всех selected cases.
    local aggregate_status="$1"
    # Нулевой status всё равно требует human review.
    local final_outcome="MANUAL REVIEW REQUIRED"
    # Любой runtime failure публикуется как FAIL, а не misleading manual success.
    if [[ "${aggregate_status}" != "${SUCCESS_EXIT_CODE}" ]]; then
        final_outcome="FAIL"
    fi
    # Footer добавляется только из bounded constant vocabulary.
    {
        printf '\n## Runner outcome\n\n'
        printf 'Outcome: %s\n' "${final_outcome}"
    } >>"${report_path}"
}
