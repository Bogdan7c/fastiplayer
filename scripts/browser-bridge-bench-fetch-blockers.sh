#!/usr/bin/env bash
# Скачивает блокировщики рекламы для матрицы `blocker-matrix` стенда моста (сессия 09c).
#   scripts/browser-bridge-bench-fetch-blockers.sh
# Только официальные релизы на GitHub, версия и SHA-256 прибиты ниже. Файлы кладутся в
# target/browser-bridge-bench/blockers/<имя>/ (каждый в свой пустой каталог) и в репозиторий
# НЕ коммитятся (каталог target/ в .gitignore). Повторный запуск ничего не качает, если каталог
# уже собран из нужной версии.
#   ublock-origin-lite  Chromium: распакованный каталог (uBlock Origin Lite, uBOL-home)
#   ublock-origin       Firefox: подписанный .xpi (uBlock Origin, gorhill/uBlock)
# Чтобы обновить версию: поменять версию и хеш ниже (хеш — digest ассета на странице релиза),
# проверить `blocker-matrix` и закоммитить новый скрипт.

# Строгий режим: недокачанный или чужой файл не должен тихо попасть в матрицу.
set -euo pipefail

script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
repo_root="$(cd -- "${script_directory}/.." >/dev/null 2>&1 && pwd)"
readonly BLOCKERS_ROOT="${repo_root}/target/browser-bridge-bench/blockers"

# uBlock Origin Lite для Chromium (MV3).
readonly UBOL_NAME="ublock-origin-lite"
readonly UBOL_VERSION="2026.1103.1743"
readonly UBOL_URL="https://github.com/uBlockOrigin/uBOL-home/releases/download/${UBOL_VERSION}/uBOLite_${UBOL_VERSION}.chromium.zip"
readonly UBOL_SHA256="79236476535b63706749e088e0ce99bb31d3d2ff15fabb4d7e990be23d6e4613"

# uBlock Origin для Firefox (подписанный .xpi).
readonly UBO_NAME="ublock-origin"
readonly UBO_VERSION="1.75.4"
readonly UBO_URL="https://github.com/gorhill/uBlock/releases/download/${UBO_VERSION}/uBlock0_${UBO_VERSION}.firefox.signed.xpi"
readonly UBO_SHA256="e2df503cce7cf5069130618c349695f62bdddb9eb8c189156d027269ae4ae267"

# Метка «этот каталог собран из этой версии и этого хеша».
readonly STAMP_FILE=".fetched-from"

for tool in curl sha256sum unzip; do
    if ! command -v "${tool}" >/dev/null 2>&1; then
        printf 'Error: %s is required\n' "${tool}" >&2
        exit 1
    fi
done

mkdir -p "${BLOCKERS_ROOT}"
staging="$(mktemp -d "${BLOCKERS_ROOT}/.staging-XXXXXX")"
# Временный каталог убирается при любом выходе, включая ошибку.
trap 'rm -rf -- "${staging}"' EXIT

# fetch_blocker <имя> <версия> <url> <sha256> <chromium-dir|firefox-xpi>
fetch_blocker() {
    local name="$1" version="$2" url="$3" expected_sha="$4" kind="$5"
    local target="${BLOCKERS_ROOT}/${name}"
    local stamp="${version} ${expected_sha}"
    if [[ -f "${target}/${STAMP_FILE}" && "$(<"${target}/${STAMP_FILE}")" == "${stamp}" ]]; then
        printf '==> %s %s already fetched\n' "${name}" "${version}" >&2
        return 0
    fi
    printf '==> fetching %s %s\n' "${name}" "${version}" >&2
    local download="${staging}/${name}.download"
    curl --fail --location --silent --show-error --output "${download}" "${url}"
    local actual_sha
    actual_sha="$(sha256sum "${download}" | cut --delimiter=' ' --fields=1)"
    if [[ "${actual_sha}" != "${expected_sha}" ]]; then
        printf 'Error: SHA-256 of %s is %s, expected %s\n' "${url}" "${actual_sha}" "${expected_sha}" >&2
        exit 1
    fi
    # Свой пустой каталог на каждый блокировщик: старое содержимое убираем целиком.
    local fresh="${staging}/${name}"
    mkdir -p "${fresh}"
    case "${kind}" in
        chromium-dir)
            unzip -q "${download}" -d "${fresh}"
            if [[ ! -f "${fresh}/manifest.json" ]]; then
                printf 'Error: %s has no manifest.json at the top of the archive\n' "${url}" >&2
                exit 1
            fi
            ;;
        firefox-xpi)
            cp -- "${download}" "${fresh}/${name}-${version}.xpi"
            ;;
        *)
            printf 'Error: unknown kind %s\n' "${kind}" >&2
            exit 1
            ;;
    esac
    printf '%s' "${stamp}" >"${fresh}/${STAMP_FILE}"
    rm -rf -- "${target}"
    mv -- "${fresh}" "${target}"
    printf '    -> %s\n' "${target}" >&2
}

fetch_blocker "${UBOL_NAME}" "${UBOL_VERSION}" "${UBOL_URL}" "${UBOL_SHA256}" chromium-dir
fetch_blocker "${UBO_NAME}" "${UBO_VERSION}" "${UBO_URL}" "${UBO_SHA256}" firefox-xpi
printf 'Blockers are in %s\n' "${BLOCKERS_ROOT}" >&2
