#!/usr/bin/env bash
# Собирает локальный dynamic LGPL FFmpeg для будущего `video-ffmpeg` tooling.
# Скрипт намеренно не подключает FFmpeg к Rust workspace и runtime playback.

# Строгий режим останавливает сборку на первой ошибке и не даёт терять сбои pipeline.
set -Eeuo pipefail

# Успешный код вынесен в константу, чтобы контракт завершения был явным.
readonly SUCCESS_EXIT_CODE=0

# Ошибка входных параметров отделена от ошибок внешних build-команд.
readonly USAGE_ERROR_EXIT_CODE=2

# Текущий target этой подготовительной сессии - стабильная ветка FFmpeg 8.1.x.
readonly DEFAULT_FFMPEG_VERSION="8.1.1"

# Отдельный каталог внутри target не попадает в Git и не требует root-доступа.
readonly DEFAULT_PREFIX_ROOT_NAME="rustiplayer-ffmpeg"

# Функция печатает ошибку в stderr с единым префиксом.
print_error() {
    # Сообщение передается первым аргументом, чтобы caller называл конкретную причину.
    local error_message="$1"

    # stderr отделяет диагностику от обычного вывода help/dry-run.
    printf 'Ошибка: %s\n' "${error_message}" >&2
}

# Функция завершает скрипт при некорректном использовании CLI или env vars.
fail_usage() {
    # Сообщение передается первым аргументом и сразу попадает пользователю.
    local error_message="$1"

    # Ошибка печатается до подсказки, чтобы было видно, что именно исправлять.
    print_error "${error_message}"

    # Короткая подсказка ведет к полному help без перегрузки stderr.
    printf 'Запустите %s --help для списка параметров.\n' "${0}" >&2

    # Отдельный код помогает отличать usage error от падения configure/make.
    exit "${USAGE_ERROR_EXIT_CODE}"
}

# Функция определяет число параллельных job-ов без обязательной зависимости от nproc.
detect_default_jobs() {
    # nproc есть на большинстве Linux-систем и даёт разумный default для make.
    if command -v nproc >/dev/null 2>&1; then
        # Команда возвращает только число, подходящее для `make -j`.
        nproc

        # После успешного nproc fallback не нужен.
        return
    fi

    # Минимальный fallback сохраняет переносимость скрипта.
    printf '1\n'
}

# Функция нормализует boolean env var или CLI-флаг в `0`/`1`.
normalize_boolean() {
    # Значение передается первым аргументом, потому что имя переменной может отличаться.
    local raw_value="$1"

    # Поддерживаем привычные формы, чтобы env vars были удобны в shell.
    case "${raw_value}" in
        1 | true | TRUE | yes | YES | on | ON)
            # `1` означает, что опциональная часть сборки включена.
            printf '1\n'
            ;;
        0 | false | FALSE | no | NO | off | OFF)
            # `0` означает, что опциональная часть сборки выключена.
            printf '0\n'
            ;;
        *)
            # Непонятное значение лучше остановить явно, чем молча выбрать default.
            fail_usage "ожидался boolean 0/1/true/false/yes/no/on/off, получено '${raw_value}'"
            ;;
    esac
}

# Функция проверяет, что версия остаётся в согласованной ветке 8.1.x.
validate_ffmpeg_version() {
    # Версия передается первым аргументом, чтобы проверку можно было тестировать отдельно.
    local requested_version="$1"

    # Эта сессия закрепляет stable 8.1.x; другие ветки требуют отдельного решения.
    case "${requested_version}" in
        8.1 | 8.1.[0-9]*)
            # Версия подходит, дополнительных действий не требуется.
            return
            ;;
        *)
            # Ошибка не даёт случайно перейти на другую ABI/API ветку.
            fail_usage "FFmpeg version '${requested_version}' не относится к stable 8.1.x"
            ;;
    esac
}

# Функция проверяет, что путь к source tree похож на распакованный FFmpeg.
validate_source_directory() {
    # Каталог source tree передается первым аргументом.
    local source_directory="$1"

    # `configure` является входной точкой сборки FFmpeg из исходников.
    if [[ ! -x "${source_directory}/configure" ]]; then
        # Без configure продолжать нельзя: make будет падать не по сути задачи.
        fail_usage "в '${source_directory}' не найден исполняемый FFmpeg configure"
    fi
}

# Функция печатает команду так, чтобы её можно было скопировать из dry-run.
print_shell_command() {
    # Первый аргумент и остальные элементы формируют argv без shell-склейки.
    local command_part

    # Префикс `+` визуально отделяет команды от поясняющего текста.
    printf '+'

    # Каждый argv-элемент quoted через %q, чтобы пробелы в путях были безопасны.
    for command_part in "$@"; do
        # Пробел перед каждым элементом делает вывод похожим на trace shell.
        printf ' %q' "${command_part}"
    done

    # Каждая команда завершается переводом строки.
    printf '\n'
}

# Функция печатает команду и выполняет её, если это не dry-run.
run_command() {
    # Команда всегда печатается в stderr, чтобы stdout можно было использовать для return values.
    print_shell_command "$@" >&2

    # В обычном режиме argv запускается без shell, чтобы не ломать пути с пробелами.
    "$@"
}

# Функция проверяет наличие внешней команды до начала реальной сборки.
require_command() {
    # Имя команды передается первым аргументом для переиспользования проверки.
    local required_command="$1"

    # command -v проверяет PATH без запуска самой команды.
    if ! command -v "${required_command}" >/dev/null 2>&1; then
        # Ошибка содержит точное имя отсутствующего инструмента.
        print_error "команда '${required_command}' не найдена в PATH"

        # Продолжать нельзя, потому что build path точно упадёт.
        exit 1
    fi
}

# Функция печатает полный help для локального build tooling.
print_help() {
    # Help живёт рядом с parser-ом, чтобы CLI и документация не расходились.
    cat <<'USAGE'
Usage: scripts/tooling/build-ffmpeg-lgpl.sh [options]

Build FFmpeg 8.1.x as local shared LGPL libav* tooling for rustiplayer.
This does not add FFmpeg to Cargo workspace and does not make playback depend on it.

Options:
  -h, --help                    Show this help.
      --dry-run                 Print planned commands without downloading/building.
      --version VERSION         FFmpeg stable 8.1.x version (default: 8.1.1).
      --prefix PATH             Install prefix (default: target/rustiplayer-ffmpeg/VERSION).
      --work-dir PATH           Build/cache directory (default: target/rustiplayer-ffmpeg/build).
      --source-dir PATH         Use an already unpacked FFmpeg source tree.
      --source-archive PATH     Use an existing ffmpeg-VERSION.tar.xz archive.
      --url URL                 Download source archive from URL.
      --jobs N                  Parallel make jobs.
      --enable-swresample       Build libswresample for future header/build needs.
      --disable-swresample      Explicitly skip libswresample (default).
      --enable-swscale          Build libswscale for future header/build needs.
      --disable-swscale         Explicitly skip libswscale (default).

Environment:
  RUSTIPLAYER_FFMPEG_VERSION
  RUSTIPLAYER_FFMPEG_PREFIX
  RUSTIPLAYER_FFMPEG_WORK_DIR
  RUSTIPLAYER_FFMPEG_SOURCE_DIR
  RUSTIPLAYER_FFMPEG_SOURCE_ARCHIVE
  RUSTIPLAYER_FFMPEG_URL
  RUSTIPLAYER_FFMPEG_JOBS
  RUSTIPLAYER_FFMPEG_ENABLE_SWRESAMPLE
  RUSTIPLAYER_FFMPEG_ENABLE_SWSCALE
USAGE
}

# Функция печатает итоговый план сборки в человекочитаемом виде.
print_build_plan() {
    # План нужен и для dry-run, и для обычного запуска перед длинной сборкой.
    printf 'FFmpeg version: %s\n' "${ffmpeg_version}"

    # Prefix является единственным местом установки локальных shared libraries.
    printf 'Install prefix: %s\n' "${ffmpeg_prefix}"

    # Work dir содержит downloads, распакованные исходники и build directory.
    printf 'Work directory: %s\n' "${work_directory}"

    # URL показывается даже при source-dir, чтобы default был виден пользователю.
    printf 'Source URL: %s\n' "${download_url}"

    # Optional libs явно видны, потому что они не являются playback conversion path.
    printf 'libswresample: %s\n' "${enable_swresample}"

    # libswscale тоже opt-in: CPU video conversion в playback path запрещён.
    printf 'libswscale: %s\n' "${enable_swscale}"

    # Jobs влияют только на скорость make, не на состав артефактов.
    printf 'Make jobs: %s\n' "${make_jobs}"
}

# Функция добавляет FFmpeg configure option с понятным именем массива.
add_configure_option() {
    # Опция передается первым аргументом и сохраняется как отдельный argv-element.
    local configure_option="$1"

    # Массив не использует строковую склейку, чтобы `--prefix=/path with spaces` был корректен.
    configure_options+=("${configure_option}")
}

# Функция собирает список configure options для минимального LGPL shared build.
build_configure_options() {
    # Глобальный массив очищается перед наполнением, чтобы повторный вызов был безопасен.
    configure_options=()

    # Prefix определяет install root для headers, shared libs и pkg-config files.
    add_configure_option "--prefix=${ffmpeg_prefix}"

    # Shared libraries нужны будущему dynamic LGPL linking.
    add_configure_option "--enable-shared"

    # Static libraries не собираем, чтобы не смешивать LGPL tooling со static link policy.
    add_configure_option "--disable-static"

    # Runtime FFmpeg CLI tools этому проекту не нужны.
    add_configure_option "--disable-programs"

    # Документация FFmpeg не нужна для локального prefix и заметно увеличивает build surface.
    add_configure_option "--disable-doc"

    # GPL отключается явно: подготовительный tooling должен оставаться LGPL by default.
    add_configure_option "--disable-gpl"

    # Nonfree отключается явно, чтобы случайная внешняя библиотека не поменяла license class.
    add_configure_option "--disable-nonfree"

    # Автодетект внешних библиотек выключен для воспроизводимого LGPL build surface.
    add_configure_option "--disable-autodetect"

    # libavutil нужен libavcodec и будущему FFI boundary.
    add_configure_option "--enable-avutil"

    # libavcodec является единственной обязательной FFmpeg decode library для будущего software path.
    add_configure_option "--enable-avcodec"

    # libavformat не нужен: demuxing в rustiplayer остаётся за существующими media crates.
    add_configure_option "--disable-avformat"

    # libavdevice не нужен без FFmpeg CLI/device IO.
    add_configure_option "--disable-avdevice"

    # libavfilter не нужен, потому что CPU filter/conversion pipeline не входит в playback path.
    add_configure_option "--disable-avfilter"

    # Hardware acceleration в FFmpeg запрещена дизайном: native backend владеет hw path.
    add_configure_option "--disable-hwaccels"

    # Encoders не нужны для плеера и не должны расширять build/license surface.
    add_configure_option "--disable-encoders"

    # Muxers не нужны без libavformat и без записи media.
    add_configure_option "--disable-muxers"

    # Demuxers не нужны: containers уже открываются другими crates проекта.
    add_configure_option "--disable-demuxers"

    # Protocols не нужны без FFmpeg network/container IO.
    add_configure_option "--disable-protocols"

    # Devices не нужны без capture/output FFmpeg devices.
    add_configure_option "--disable-devices"

    # Filters не нужны, потому что color conversion/tone mapping остаются GPU-side.
    add_configure_option "--disable-filters"

    # PIC делает shared build явным на платформах, где configure это учитывает.
    add_configure_option "--enable-pic"

    # libswresample опционален и выключен по умолчанию.
    if [[ "${enable_swresample}" == "1" ]]; then
        # Включаем только по явному запросу для будущих header/build experiments.
        add_configure_option "--enable-swresample"
    else
        # По умолчанию аудио resampling не попадает в prefix.
        add_configure_option "--disable-swresample"
    fi

    # libswscale опционален и выключен по умолчанию.
    if [[ "${enable_swscale}" == "1" ]]; then
        # Включение не разрешает CPU conversion в playback path, только build/header needs.
        add_configure_option "--enable-swscale"
    else
        # По умолчанию video scaling/conversion library не попадает в prefix.
        add_configure_option "--disable-swscale"
    fi
}

# Функция печатает команды, которые dry-run выполнил бы в обычном режиме.
print_dry_run_commands() {
    # Dry-run не создает каталоги и не требует network/build dependencies.
    printf '\nDry-run commands:\n'

    # Install prefix создается make install, но work dirs нужны до configure.
    print_shell_command mkdir -p "${downloads_directory}" "${sources_directory}" "${build_directory}"

    # Если source-dir задан, download/extract пропускаются.
    if [[ -n "${source_directory}" ]]; then
        # Показываем проверку source tree как комментарий для пользователя.
        printf '# source-dir будет использован без download/extract: %s\n' "${source_directory}"
    else
        # Если archive задан, curl не нужен.
        if [[ -n "${source_archive}" ]]; then
            # Существующий archive позволяет работать без сети.
            printf '# source-archive будет использован без download: %s\n' "${source_archive}"
        else
            # Default path для downloaded archive живёт внутри target.
            print_shell_command curl -L --fail --output "${resolved_archive_path}" "${download_url}"
        fi

        # Распаковка создает source tree, если его ещё нет.
        print_shell_command tar -xf "${resolved_archive_path}" -C "${sources_directory}"
    fi

    # Configure вызывается из отдельного build directory.
    print_shell_command "${resolved_source_directory}/configure" "${configure_options[@]}"

    # make использует выбранное число job-ов.
    print_shell_command make "-j${make_jobs}"

    # make install кладет headers/libs/pkg-config files в prefix.
    print_shell_command make install

    # pkg-config probe проверяет установленные .pc files без подключения к runtime.
    print_pkg_config_probe_command
}

# Функция готовит source tree: использует source-dir, archive или скачивает tarball.
prepare_source_tree() {
    # Явный source-dir полезен для локальных экспериментов без download.
    if [[ -n "${source_directory}" ]]; then
        # Проверяем configure до запуска build.
        validate_source_directory "${source_directory}"

        # Печатаем путь, чтобы caller мог использовать результат.
        printf '%s\n' "${source_directory}"

        # Остальная подготовка source не нужна.
        return
    fi

    # Каталоги создаются только в real-run, dry-run их не трогает.
    run_command mkdir -p "${downloads_directory}" "${sources_directory}" "${build_directory}"

    # Если archive не задан, используем download cache внутри target.
    if [[ -z "${source_archive}" ]]; then
        # Скачивание пропускается, если archive уже есть.
        if [[ ! -f "${resolved_archive_path}" ]]; then
            # curl запускается с --fail, чтобы HTTP errors не превращались в HTML tarball.
            run_command curl -L --fail --output "${resolved_archive_path}" "${download_url}"
        fi
    fi

    # Распакованный source tree переиспользуется, чтобы не удалять локальные build artifacts.
    if [[ ! -x "${resolved_source_directory}/configure" ]]; then
        # tar распаковывает официальный архив в sources directory.
        run_command tar -xf "${resolved_archive_path}" -C "${sources_directory}"
    fi

    # Проверяем итоговый source tree независимо от того, как он был получен.
    validate_source_directory "${resolved_source_directory}"

    # Печатаем путь, чтобы caller мог использовать результат.
    printf '%s\n' "${resolved_source_directory}"
}

# Функция строит и устанавливает FFmpeg из подготовленного source tree.
build_and_install_ffmpeg() {
    # Source tree передается первым аргументом, чтобы real-run не зависел от global mutation.
    local prepared_source_directory="$1"

    # Build directory создается отдельно от source tree.
    run_command mkdir -p "${build_directory}"

    # Переход в build directory нужен для out-of-tree configure.
    cd "${build_directory}"

    # Configure получает массив argv, а не одну строку с shell parsing.
    run_command "${prepared_source_directory}/configure" "${configure_options[@]}"

    # make компилирует FFmpeg shared libraries.
    run_command make "-j${make_jobs}"

    # make install переносит headers, shared libs и .pc files в prefix.
    run_command make install
}

# Функция собирает список pkg-config packages, которые должны появиться в prefix.
build_pkg_config_packages() {
    # libavutil и libavcodec являются обязательным результатом текущего tooling.
    pkg_config_packages=(libavutil libavcodec)

    # libswresample проверяется только если пользователь явно включил его сборку.
    if [[ "${enable_swresample}" == "1" ]]; then
        # Имя package соответствует FFmpeg .pc файлу.
        pkg_config_packages+=(libswresample)
    fi

    # libswscale проверяется только если пользователь явно включил его сборку.
    if [[ "${enable_swscale}" == "1" ]]; then
        # Имя package соответствует FFmpeg .pc файлу.
        pkg_config_packages+=(libswscale)
    fi
}

# Функция печатает pkg-config probe с корректным PKG_CONFIG_PATH.
print_pkg_config_probe_command() {
    # Первый каталог .pc files находится внутри install prefix.
    local prefix_pkg_config_path="${ffmpeg_prefix}/lib/pkgconfig"

    # Если у пользователя уже есть PKG_CONFIG_PATH, добавляем его после локального prefix.
    local combined_pkg_config_path="${prefix_pkg_config_path}${PKG_CONFIG_PATH:+:${PKG_CONFIG_PATH}}"

    # env используется явно, чтобы команда не зависела от shell export.
    print_shell_command env "PKG_CONFIG_PATH=${combined_pkg_config_path}" pkg-config --modversion "${pkg_config_packages[@]}"
}

# Функция проверяет установленные pkg-config файлы и версии FFmpeg libraries.
probe_installed_libraries() {
    # Probe относится к tooling/install verification, а не к startup/runtime capability.
    printf '\nПроверяю установленные FFmpeg .pc files через pkg-config:\n'

    # Команда печатается перед выполнением для прозрачности.
    print_pkg_config_probe_command

    # Локальный prefix приоритетнее системного FFmpeg.
    PKG_CONFIG_PATH="${ffmpeg_prefix}/lib/pkgconfig${PKG_CONFIG_PATH:+:${PKG_CONFIG_PATH}}" \
        pkg-config --modversion "${pkg_config_packages[@]}"
}

# Функция печатает env vars, которые нужны будущему build script или ручной проверке.
print_environment_exports() {
    # Пустая строка отделяет итог сборки от export подсказок.
    printf '\nДля будущих build/probe экспериментов используйте:\n'

    # PKG_CONFIG_PATH нужен будущему Rust build.rs/pkg-config lookup.
    printf 'export PKG_CONFIG_PATH=%q\n' "${ffmpeg_prefix}/lib/pkgconfig${PKG_CONFIG_PATH:+:${PKG_CONFIG_PATH}}"

    # LD_LIBRARY_PATH нужен локальному запуску binaries/tests с dynamic FFmpeg libs.
    printf 'export LD_LIBRARY_PATH=%q\n' "${ffmpeg_prefix}/lib${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"

    # Guardrail формулируется здесь, потому что это самый частый источник путаницы.
    printf '\nGuardrail: этот prefix не делает rustiplayer runtime-зависимым от FFmpeg.\n'
}

# Функция разбирает CLI options и выставляет runtime variables.
parse_arguments() {
    # Цикл обрабатывает только named options, чтобы случайный positional argument был ошибкой.
    while (($# > 0)); do
        # Каждый option разбирается отдельно, чтобы ошибки указывали точный флаг.
        case "$1" in
            -h | --help)
                # Help завершается успешно и не требует валидации остальных env vars.
                print_help

                # Выход сразу предотвращает побочные эффекты.
                exit "${SUCCESS_EXIT_CODE}"
                ;;
            --dry-run)
                # Dry-run печатает план и команды без скачивания/сборки.
                dry_run="1"

                # Переходим к следующему аргументу.
                shift
                ;;
            --version)
                # У version должен быть следующий аргумент.
                [[ $# -ge 2 ]] || fail_usage "--version требует значение"

                # Новая версия сохраняется до финальной валидации.
                ffmpeg_version="$2"

                # CLI version должна обновить default URL/prefix, если они не заданы явно.
                shift 2
                ;;
            --prefix)
                # У prefix должен быть следующий аргумент.
                [[ $# -ge 2 ]] || fail_usage "--prefix требует путь"

                # CLI prefix считается явным и не будет перезаписан version default-ом.
                ffmpeg_prefix="$2"

                # Флаг нужен, чтобы default prefix мог зависеть от версии.
                ffmpeg_prefix_is_explicit="1"

                # Переходим к следующей паре аргументов.
                shift 2
                ;;
            --work-dir)
                # У work-dir должен быть следующий аргумент.
                [[ $# -ge 2 ]] || fail_usage "--work-dir требует путь"

                # Work dir хранит downloads/source/build cache.
                work_directory="$2"

                # Переходим к следующей паре аргументов.
                shift 2
                ;;
            --source-dir)
                # У source-dir должен быть следующий аргумент.
                [[ $# -ge 2 ]] || fail_usage "--source-dir требует путь"

                # Source dir отключает download/extract path.
                source_directory="$2"

                # Переходим к следующей паре аргументов.
                shift 2
                ;;
            --source-archive)
                # У source-archive должен быть следующий аргумент.
                [[ $# -ge 2 ]] || fail_usage "--source-archive требует путь"

                # Source archive позволяет работать без сети.
                source_archive="$2"

                # Переходим к следующей паре аргументов.
                shift 2
                ;;
            --url)
                # У url должен быть следующий аргумент.
                [[ $# -ge 2 ]] || fail_usage "--url требует значение"

                # Явный URL нужен для mirrors или предварительно проверенного архива.
                download_url="$2"

                # Флаг защищает URL от пересборки после --version.
                download_url_is_explicit="1"

                # Переходим к следующей паре аргументов.
                shift 2
                ;;
            --jobs)
                # У jobs должен быть следующий аргумент.
                [[ $# -ge 2 ]] || fail_usage "--jobs требует число"

                # Значение проверяется после parsing вместе с env default.
                make_jobs="$2"

                # Переходим к следующей паре аргументов.
                shift 2
                ;;
            --enable-swresample)
                # Явный opt-in включает libswresample.
                enable_swresample="1"

                # Переходим к следующему аргументу.
                shift
                ;;
            --disable-swresample)
                # Явный opt-out оставляет libswresample вне prefix.
                enable_swresample="0"

                # Переходим к следующему аргументу.
                shift
                ;;
            --enable-swscale)
                # Явный opt-in включает libswscale.
                enable_swscale="1"

                # Переходим к следующему аргументу.
                shift
                ;;
            --disable-swscale)
                # Явный opt-out оставляет libswscale вне prefix.
                enable_swscale="0"

                # Переходим к следующему аргументу.
                shift
                ;;
            *)
                # Неизвестные positional/options лучше остановить сразу.
                fail_usage "неизвестный параметр '$1'"
                ;;
        esac
    done
}

# Функция синхронизирует defaults, которые зависят от итоговой версии.
resolve_version_dependent_defaults() {
    # Если prefix не задан явно, версия участвует в default install path.
    if [[ "${ffmpeg_prefix_is_explicit}" == "0" ]]; then
        # target/ уже игнорируется Git, поэтому локальная сборка не загрязняет repo.
        ffmpeg_prefix="${repo_root}/target/${DEFAULT_PREFIX_ROOT_NAME}/${ffmpeg_version}"
    fi

    # Если URL не задан явно, версия участвует в официальном release URL.
    if [[ "${download_url_is_explicit}" == "0" ]]; then
        # Официальный архив лежит в ffmpeg.org/releases.
        download_url="https://ffmpeg.org/releases/ffmpeg-${ffmpeg_version}.tar.xz"
    fi
}

# Функция вычисляет производные пути после parsing и validation.
resolve_paths() {
    # Downloads cache отделен от source/build каталогов.
    downloads_directory="${work_directory}/downloads"

    # Распакованные source trees живут рядом и могут переиспользоваться.
    sources_directory="${work_directory}/sources"

    # Build directory включает версию, чтобы разные версии не смешивали object files.
    build_directory="${work_directory}/build-ffmpeg-${ffmpeg_version}"

    # Default archive path используется, если source-archive не задан.
    resolved_archive_path="${source_archive:-${downloads_directory}/ffmpeg-${ffmpeg_version}.tar.xz}"

    # Default source tree соответствует имени каталога внутри официального tarball.
    resolved_source_directory="${source_directory:-${sources_directory}/ffmpeg-${ffmpeg_version}}"
}

# Функция проверяет входные значения, которые нельзя оставить configure/make.
validate_inputs() {
    # Версия фиксируется на stable 8.1.x для текущего этапа дизайна.
    validate_ffmpeg_version "${ffmpeg_version}"

    # Boolean env vars нормализуются после CLI parsing.
    enable_swresample="$(normalize_boolean "${enable_swresample}")"

    # Boolean env vars нормализуются после CLI parsing.
    enable_swscale="$(normalize_boolean "${enable_swscale}")"

    # make -j требует положительное целое число.
    if [[ ! "${make_jobs}" =~ ^[1-9][0-9]*$ ]]; then
        # Нечисловой jobs лучше поймать до запуска make.
        fail_usage "--jobs/RUSTIPLAYER_FFMPEG_JOBS должен быть положительным целым числом"
    fi

    # source-dir и source-archive одновременно создают неоднозначный source of truth.
    if [[ -n "${source_directory}" && -n "${source_archive}" ]]; then
        # Останавливаемся, чтобы пользователь явно выбрал один источник.
        fail_usage "нельзя одновременно задавать --source-dir и --source-archive"
    fi
}

# Функция проверяет реальные зависимости только перед real-run.
require_real_run_commands() {
    # make нужен для компиляции FFmpeg.
    require_command "make"

    # pkg-config нужен для install/probe результата.
    require_command "pkg-config"

    # tar нужен для официального source archive, если source-dir не задан.
    if [[ -z "${source_directory}" ]]; then
        # tar распаковывает .tar.xz.
        require_command "tar"
    fi

    # curl нужен только если archive не задан и default cache ещё может отсутствовать.
    if [[ -z "${source_directory}" && -z "${source_archive}" && ! -f "${resolved_archive_path}" ]]; then
        # curl скачивает официальный release tarball.
        require_command "curl"
    fi
}

# Главная функция связывает parsing, planning, build и tooling probe.
main() {
    # Каталог скрипта нужен, чтобы default paths не зависели от cwd.
    local script_directory

    # Подготовленный source tree возвращается функцией `prepare_source_tree`.
    local prepared_source_directory

    # Вычисляем абсолютный путь к scripts/tooling.
    script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"

    # Корень repo находится на два уровня выше scripts/tooling.
    repo_root="$(cd -- "${script_directory}/../.." >/dev/null 2>&1 && pwd)"

    # Env var version имеет приоритет над hardcoded default.
    ffmpeg_version="${RUSTIPLAYER_FFMPEG_VERSION:-${DEFAULT_FFMPEG_VERSION}}"

    # Env prefix считается явным и не будет изменён после --version.
    ffmpeg_prefix_is_explicit="$([[ -n "${RUSTIPLAYER_FFMPEG_PREFIX:-}" ]] && printf '1' || printf '0')"

    # Prefix из env берется сразу; default заполнится после parsing.
    ffmpeg_prefix="${RUSTIPLAYER_FFMPEG_PREFIX:-}"

    # Work dir по умолчанию лежит внутри ignored target.
    work_directory="${RUSTIPLAYER_FFMPEG_WORK_DIR:-${repo_root}/target/${DEFAULT_PREFIX_ROOT_NAME}/build}"

    # Source dir может указывать на уже распакованный FFmpeg checkout/tarball.
    source_directory="${RUSTIPLAYER_FFMPEG_SOURCE_DIR:-}"

    # Source archive может указывать на заранее скачанный ffmpeg-VERSION.tar.xz.
    source_archive="${RUSTIPLAYER_FFMPEG_SOURCE_ARCHIVE:-}"

    # Env URL считается явным и не будет изменён после --version.
    download_url_is_explicit="$([[ -n "${RUSTIPLAYER_FFMPEG_URL:-}" ]] && printf '1' || printf '0')"

    # URL из env берется сразу; default заполнится после parsing.
    download_url="${RUSTIPLAYER_FFMPEG_URL:-}"

    # Jobs из env или CPU count управляют только скоростью make.
    make_jobs="${RUSTIPLAYER_FFMPEG_JOBS:-$(detect_default_jobs)}"

    # libswresample выключен по умолчанию.
    enable_swresample="${RUSTIPLAYER_FFMPEG_ENABLE_SWRESAMPLE:-0}"

    # libswscale выключен по умолчанию.
    enable_swscale="${RUSTIPLAYER_FFMPEG_ENABLE_SWSCALE:-0}"

    # Dry-run выключен по умолчанию.
    dry_run="0"

    # CLI options имеют приоритет над env defaults.
    parse_arguments "$@"

    # Default prefix/URL зависят от финальной версии.
    resolve_version_dependent_defaults

    # Проверяем входные значения до вычисления build plan.
    validate_inputs

    # Производные пути нужны и dry-run, и real-run.
    resolve_paths

    # Configure options собираются после validation, чтобы boolean values были нормализованы.
    build_configure_options

    # pkg-config package list отражает обязательные и opt-in libraries.
    build_pkg_config_packages

    # План печатается до потенциально долгой сборки.
    print_build_plan

    # Dry-run завершает работу без filesystem/network/build effects.
    if [[ "${dry_run}" == "1" ]]; then
        # Печатаем команды, которые были бы выполнены.
        print_dry_run_commands

        # Успешный dry-run является тестируемым контрактом скрипта.
        exit "${SUCCESS_EXIT_CODE}"
    fi

    # Реальный запуск проверяет внешние команды до начала работы.
    require_real_run_commands

    # Source tree готовится через source-dir, archive или download.
    prepared_source_directory="$(prepare_source_tree)"

    # FFmpeg собирается и устанавливается в prefix.
    build_and_install_ffmpeg "${prepared_source_directory}"

    # Tooling-level probe проверяет версии installed libraries.
    probe_installed_libraries

    # Финальная подсказка показывает build-time env vars.
    print_environment_exports
}

# Запуск main сохраняет функции пригодными для будущего shell-test harness.
main "$@"

# Явное успешное завершение делает контракт скрипта очевидным.
exit "${SUCCESS_EXIT_CODE}"
