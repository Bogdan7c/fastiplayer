#!/usr/bin/env bash
# Скрипт устанавливает SonarQube Community Build и SonarScanner CLI системно без Docker.

# Строгий режим останавливает установку при первой ошибке, unset-переменной или сбое в pipeline.
set -Eeuo pipefail

# Версия SonarQube вынесена в переменную, чтобы обновление не требовало правки логики.
readonly SONARQUBE_VERSION="${SONARQUBE_VERSION:-26.5.0.122743}"

# Версия SonarScanner CLI вынесена отдельно, потому что scanner обновляется независимо от сервера.
readonly SONAR_SCANNER_VERSION="${SONAR_SCANNER_VERSION:-8.0.1.6346}"

# Системный пользователь изолирует сервер от обычного пользователя разработчика.
readonly SONARQUBE_USER="${SONARQUBE_USER:-sonarqube}"

# Системная группа совпадает с пользователем, чтобы права на каталоги были очевидными.
readonly SONARQUBE_GROUP="${SONARQUBE_GROUP:-sonarqube}"

# Java 21 выбрана как поддерживаемый runtime для ZIP-запуска SonarQube.
readonly JAVA_HOME_PATH="${JAVA_HOME_PATH:-/usr/lib/jvm/java-21-openjdk}"

# Симлинк приложения остается стабильным для systemd unit.
readonly SONARQUBE_HOME_LINK="${SONARQUBE_HOME_LINK:-/opt/sonarqube}"

# Версионированный каталог позволяет не затирать произвольную существующую установку.
readonly SONARQUBE_INSTALL_DIR="${SONARQUBE_INSTALL_DIR:-/opt/sonarqube-${SONARQUBE_VERSION}}"

# Симлинк scanner остается стабильным для PATH.
readonly SONAR_SCANNER_HOME_LINK="${SONAR_SCANNER_HOME_LINK:-/opt/sonar-scanner}"

# Версионированный каталог scanner упрощает будущие обновления.
readonly SONAR_SCANNER_INSTALL_DIR="${SONAR_SCANNER_INSTALL_DIR:-/opt/sonar-scanner-${SONAR_SCANNER_VERSION}-linux-x64}"

# Данные H2 вынесены из /opt, чтобы обновление приложения не трогало состояние.
readonly SONARQUBE_DATA_DIR="${SONARQUBE_DATA_DIR:-/var/lib/sonarqube/data}"

# Логи вынесены в стандартный системный каталог.
readonly SONARQUBE_LOG_DIR="${SONARQUBE_LOG_DIR:-/var/log/sonarqube}"

# Temp-каталог вынесен в /var/cache, чтобы не смешивать временные файлы с данными.
readonly SONARQUBE_TEMP_DIR="${SONARQUBE_TEMP_DIR:-/var/cache/sonarqube/temp}"

# systemd unit ставится в системный каталог units.
readonly SONARQUBE_SERVICE_PATH="${SONARQUBE_SERVICE_PATH:-/etc/systemd/system/sonarqube.service}"

# Временный каталог хранит ZIP-файлы и распакованные архивы только на время установки.
readonly INSTALL_WORK_DIR="${INSTALL_WORK_DIR:-/tmp/fastiplayer-sonarqube-install}"

# Официальный ZIP SonarQube берется из CDN SonarSource.
readonly SONARQUBE_ZIP_URL="https://binaries.sonarsource.com/Distribution/sonarqube/sonarqube-${SONARQUBE_VERSION}.zip"

# Официальный ZIP SonarScanner CLI берется из CDN SonarSource.
readonly SONAR_SCANNER_ZIP_URL="https://binaries.sonarsource.com/Distribution/sonar-scanner-cli/sonar-scanner-cli-${SONAR_SCANNER_VERSION}-linux-x64.zip"

# Локальный путь ZIP SonarQube нужен для повторного запуска без повторного скачивания.
readonly SONARQUBE_ZIP_PATH="${INSTALL_WORK_DIR}/sonarqube-${SONARQUBE_VERSION}.zip"

# Локальный путь ZIP scanner нужен для повторного запуска без повторного скачивания.
readonly SONAR_SCANNER_ZIP_PATH="${INSTALL_WORK_DIR}/sonar-scanner-cli-${SONAR_SCANNER_VERSION}-linux-x64.zip"

# Функция печатает информационное сообщение в stderr.
print_info() {
    # Текст сообщения передается первым аргументом.
    local info_message="$1"

    # stderr оставляет stdout свободным для возможной автоматизации.
    printf '%s\n' "${info_message}" >&2
}

# Функция печатает ошибку в stderr.
print_error() {
    # Текст ошибки передается первым аргументом.
    local error_message="$1"

    # Единый префикс делает ошибки заметными в длинном логе установки.
    printf 'Ошибка: %s\n' "${error_message}" >&2
}

# Функция завершает установку с понятной причиной.
fail_installation() {
    # Причина остановки передается первым аргументом.
    local failure_message="$1"

    # Печатаем причину перед выходом.
    print_error "${failure_message}"

    # Ненулевой код нужен для CI или shell-автоматизации.
    exit 1
}

# Функция проверяет, что скрипт запущен от root.
require_root() {
    # EUID равен нулю только у root-процесса.
    if [[ "${EUID}" -ne 0 ]]; then
        # Установка пишет в /opt, /var и /etc/systemd/system, поэтому без root продолжать нельзя.
        fail_installation "запустите скрипт через sudo: sudo scripts/install-sonarqube-system.sh"
    fi
}

# Функция проверяет, что система похожа на Arch/CachyOS.
require_arch_like_system() {
    # pacman-команды ниже корректны только для Arch-like систем.
    if ! grep -Eq '^(ID|ID_LIKE)=.*(arch|cachyos)' /etc/os-release; then
        # Сообщение объясняет, почему установка остановлена до package manager.
        fail_installation "этот installer ожидает Arch/CachyOS с pacman"
    fi
}

# Функция ставит системные зависимости через pacman.
install_system_packages() {
    # Сообщение фиксирует начало потенциально долгой операции.
    print_info "Устанавливаю системные зависимости: jdk21-openjdk unzip curl"

    # --needed не переустанавливает уже установленные пакеты.
    pacman -S --needed --noconfirm jdk21-openjdk unzip curl
}

# Функция создает системную группу SonarQube.
ensure_sonarqube_group() {
    # Если группа уже существует, повторно создавать ее не нужно.
    if getent group "${SONARQUBE_GROUP}" >/dev/null 2>&1; then
        # Сообщение полезно при повторном запуске installer.
        print_info "Группа ${SONARQUBE_GROUP} уже существует"

        # Возвращаемся без изменения существующей группы.
        return
    fi

    # Сообщение фиксирует создание изолированной группы.
    print_info "Создаю системную группу ${SONARQUBE_GROUP}"

    # --system создает системную группу с корректным диапазоном GID.
    groupadd --system "${SONARQUBE_GROUP}"
}

# Функция создает системного пользователя.
ensure_sonarqube_user() {
    # Если пользователь уже существует, повторно создавать его не нужно.
    if id -u "${SONARQUBE_USER}" >/dev/null 2>&1; then
        # Сообщение полезно при повторном запуске installer.
        print_info "Пользователь ${SONARQUBE_USER} уже существует"

        # Возвращаемся без изменения существующей учетной записи.
        return
    fi

    # Сообщение фиксирует создание изолированного пользователя.
    print_info "Создаю системного пользователя ${SONARQUBE_USER}"

    # --system создает системную учетную запись без интерактивного shell.
    useradd --system --gid "${SONARQUBE_GROUP}" --home-dir /var/lib/sonarqube --shell /usr/bin/nologin --create-home "${SONARQUBE_USER}"
}

# Функция создает рабочий каталог установки.
prepare_install_work_dir() {
    # Каталог принадлежит root, потому что installer запускается от root.
    mkdir -p "${INSTALL_WORK_DIR}"
}

# Функция скачивает файл, если его еще нет.
download_if_missing() {
    # URL передается первым аргументом.
    local source_url="$1"

    # Путь назначения передается вторым аргументом.
    local destination_path="$2"

    # Существующий непустой файл переиспользуется для идемпотентности.
    if [[ -s "${destination_path}" ]]; then
        # Сообщение показывает, что сеть не используется повторно.
        print_info "Архив уже скачан: ${destination_path}"

        # Возвращаемся к следующему шагу.
        return
    fi

    # Сообщение показывает официальный URL без скрытой логики.
    print_info "Скачиваю ${source_url}"

    # curl падает на HTTP-ошибках и сохраняет файл только по указанному пути.
    curl --fail --location --show-error --output "${destination_path}" "${source_url}"
}

# Функция проверяет целостность ZIP через unzip.
verify_zip_archive() {
    # Путь ZIP передается первым аргументом.
    local zip_path="$1"

    # Сообщение фиксирует проверяемый архив.
    print_info "Проверяю ZIP: ${zip_path}"

    # unzip -t читает архив и возвращает ошибку при повреждении.
    unzip -t "${zip_path}" >/dev/null
}

# Функция распаковывает SonarQube в версионированный каталог.
install_sonarqube_files() {
    # Повторный запуск не должен перезаписывать уже установленную версию.
    if [[ -d "${SONARQUBE_INSTALL_DIR}" ]]; then
        # Сообщение показывает, какой каталог переиспользуется.
        print_info "SonarQube уже распакован: ${SONARQUBE_INSTALL_DIR}"
    else
        # Временный каталог распаковки удаляется перед использованием.
        rm -rf "${INSTALL_WORK_DIR}/sonarqube-extract"

        # Создаем чистую точку распаковки.
        mkdir -p "${INSTALL_WORK_DIR}/sonarqube-extract"

        # Сообщение фиксирует начало распаковки большого архива.
        print_info "Распаковываю SonarQube ${SONARQUBE_VERSION}"

        # Архив содержит корневой каталог sonarqube-<version>.
        unzip -q "${SONARQUBE_ZIP_PATH}" -d "${INSTALL_WORK_DIR}/sonarqube-extract"

        # Переносим распакованный каталог в /opt.
        mv "${INSTALL_WORK_DIR}/sonarqube-extract/sonarqube-${SONARQUBE_VERSION}" "${SONARQUBE_INSTALL_DIR}"
    fi

    # Владельцем приложения должен быть отдельный пользователь SonarQube.
    chown -R "${SONARQUBE_USER}:${SONARQUBE_GROUP}" "${SONARQUBE_INSTALL_DIR}"

    # Старый симлинк можно безопасно заменить атомарно, не заходя внутрь существующего каталога.
    ln -sfnT "${SONARQUBE_INSTALL_DIR}" "${SONARQUBE_HOME_LINK}"
}

# Функция распаковывает SonarScanner CLI в версионированный каталог.
install_sonar_scanner_files() {
    # Повторный запуск не должен перезаписывать уже установленную версию scanner.
    if [[ -d "${SONAR_SCANNER_INSTALL_DIR}" ]]; then
        # Сообщение показывает, какой каталог переиспользуется.
        print_info "SonarScanner уже распакован: ${SONAR_SCANNER_INSTALL_DIR}"
    else
        # Временный каталог распаковки удаляется перед использованием.
        rm -rf "${INSTALL_WORK_DIR}/sonar-scanner-extract"

        # Создаем чистую точку распаковки.
        mkdir -p "${INSTALL_WORK_DIR}/sonar-scanner-extract"

        # Сообщение фиксирует начало распаковки scanner.
        print_info "Распаковываю SonarScanner CLI ${SONAR_SCANNER_VERSION}"

        # Архив содержит корневой каталог sonar-scanner-<version>-linux-x64.
        unzip -q "${SONAR_SCANNER_ZIP_PATH}" -d "${INSTALL_WORK_DIR}/sonar-scanner-extract"

        # Переносим scanner в /opt.
        mv "${INSTALL_WORK_DIR}/sonar-scanner-extract/sonar-scanner-${SONAR_SCANNER_VERSION}-linux-x64" "${SONAR_SCANNER_INSTALL_DIR}"
    fi

    # Scanner может принадлежать root, потому что он только читается и запускается пользователем.
    chown -R root:root "${SONAR_SCANNER_INSTALL_DIR}"

    # Старый симлинк можно безопасно заменить атомарно, не заходя внутрь существующего каталога.
    ln -sfnT "${SONAR_SCANNER_INSTALL_DIR}" "${SONAR_SCANNER_HOME_LINK}"

    # Стабильная команда в /usr/local/bin нужна для scripts/sonar-local-analysis.sh.
    ln -sfnT "${SONAR_SCANNER_HOME_LINK}/bin/sonar-scanner" /usr/local/bin/sonar-scanner
}

# Функция создает каталоги данных, логов и temp.
prepare_runtime_directories() {
    # Каталог данных хранит H2 и состояние локального сервера.
    install -d -o "${SONARQUBE_USER}" -g "${SONARQUBE_GROUP}" -m 0750 "${SONARQUBE_DATA_DIR}"

    # Каталог логов должен быть доступен процессу SonarQube на запись.
    install -d -o "${SONARQUBE_USER}" -g "${SONARQUBE_GROUP}" -m 0750 "${SONARQUBE_LOG_DIR}"

    # Temp-каталог должен быть доступен процессу SonarQube на запись.
    install -d -o "${SONARQUBE_USER}" -g "${SONARQUBE_GROUP}" -m 0750 "${SONARQUBE_TEMP_DIR}"
}

# Функция удаляет активную строку свойства из sonar.properties.
remove_active_property() {
    # Имя свойства передается первым аргументом.
    local property_name="$1"

    # Файл настроек SonarQube расположен внутри стабильного home.
    local sonar_properties_path="${SONARQUBE_HOME_LINK}/conf/sonar.properties"

    # Точки в имени свойства должны считаться буквальными точками в sed-regex.
    local escaped_property_name="${property_name//./\\.}"

    # sed удаляет только некомментированные строки с этим ключом.
    sed -i "/^[[:space:]]*${escaped_property_name}[[:space:]]*=/d" "${sonar_properties_path}"
}

# Функция добавляет одну строку свойства в sonar.properties.
append_property() {
    # Имя свойства передается первым аргументом.
    local property_name="$1"

    # Значение свойства передается вторым аргументом.
    local property_value="$2"

    # Файл настроек SonarQube расположен внутри стабильного home.
    local sonar_properties_path="${SONARQUBE_HOME_LINK}/conf/sonar.properties"

    # printf добавляет ровно одну строку key=value.
    printf '%s=%s\n' "${property_name}" "${property_value}" >>"${sonar_properties_path}"
}

# Функция настраивает SonarQube на localhost и внешние runtime-каталоги.
configure_sonarqube_properties() {
    # Файл настроек SonarQube расположен внутри стабильного home.
    local sonar_properties_path="${SONARQUBE_HOME_LINK}/conf/sonar.properties"

    # Сообщение показывает, какой файл меняется.
    print_info "Настраиваю ${sonar_properties_path}"

    # Удаляем прежнюю активную настройку host, если installer запускается повторно.
    remove_active_property "sonar.web.host"

    # Удаляем прежнюю активную настройку port, если installer запускается повторно.
    remove_active_property "sonar.web.port"

    # Удаляем прежнюю активную настройку data path, если installer запускается повторно.
    remove_active_property "sonar.path.data"

    # Удаляем прежнюю активную настройку logs path, если installer запускается повторно.
    remove_active_property "sonar.path.logs"

    # Удаляем прежнюю активную настройку temp path, если installer запускается повторно.
    remove_active_property "sonar.path.temp"

    # Отделяем локальный блок от штатных настроек SonarQube.
    printf '\n# Локальная системная установка Fastiplayer.\n' >>"${sonar_properties_path}"

    # Веб-интерфейс слушает только loopback, чтобы локальная оценка не открывала порт в сеть.
    append_property "sonar.web.host" "127.0.0.1"

    # Порт 9000 остается стандартным для локального SonarQube.
    append_property "sonar.web.port" "9000"

    # Данные H2 лежат в /var/lib.
    append_property "sonar.path.data" "${SONARQUBE_DATA_DIR}"

    # Логи лежат в /var/log.
    append_property "sonar.path.logs" "${SONARQUBE_LOG_DIR}"

    # Temp лежит в /var/cache.
    append_property "sonar.path.temp" "${SONARQUBE_TEMP_DIR}"

    # Владелец должен сохранить право записи после изменения файла root-процессом.
    chown "${SONARQUBE_USER}:${SONARQUBE_GROUP}" "${sonar_properties_path}"
}

# Функция ставит systemd unit для SonarQube.
install_systemd_service() {
    # Сообщение показывает путь unit-файла.
    print_info "Пишу ${SONARQUBE_SERVICE_PATH}"

    # heredoc создает unit целиком, чтобы не собирать systemd-конфиг по кускам.
    cat >"${SONARQUBE_SERVICE_PATH}" <<UNIT
# Локальный SonarQube Community Build для анализа Fastiplayer.
[Unit]
# Описание видно в systemctl status.
Description=SonarQube Community Build
# Сеть должна быть поднята до старта веб-сервера.
After=network.target

[Service]
# SonarQube запускается отдельным системным пользователем.
User=${SONARQUBE_USER}
# Группа совпадает с системным пользователем.
Group=${SONARQUBE_GROUP}
# Рабочий каталог нужен штатным скриптам SonarQube.
WorkingDirectory=${SONARQUBE_HOME_LINK}
# JAVA_HOME фиксирует поддерживаемый JDK 21.
Environment=JAVA_HOME=${JAVA_HOME_PATH}
# SONAR_JAVA_PATH фиксирует конкретный бинарник java.
Environment=SONAR_JAVA_PATH=${JAVA_HOME_PATH}/bin/java
# Foreground-режим нужен systemd, чтобы отслеживать процесс.
ExecStart=${SONARQUBE_HOME_LINK}/bin/linux-x86-64/sonar.sh console
# Большой лимит файлов нужен Elasticsearch внутри SonarQube.
LimitNOFILE=131072
# Лимит процессов соответствует рекомендации из плана установки.
LimitNPROC=8192
# Перезапуск включен только при сбоях, а не при ручной остановке.
Restart=on-failure
# Таймаут старта увеличен, потому что первый запуск может быть долгим.
TimeoutStartSec=300

[Install]
# multi-user.target делает сервис обычным системным демоном.
WantedBy=multi-user.target
UNIT
}

# Функция проверяет системные лимиты перед стартом сервиса.
check_runtime_limits() {
    # vm.max_map_count критичен для Elasticsearch.
    local vm_max_map_count

    # Читаем текущее значение из sysctl.
    vm_max_map_count="$(sysctl -n vm.max_map_count)"

    # Значение ниже 524288 часто ломает запуск SonarQube.
    if [[ "${vm_max_map_count}" -lt 524288 ]]; then
        # Останавливаемся с причиной, а не маскируем будущий сбой systemd.
        fail_installation "vm.max_map_count=${vm_max_map_count}; поднимите значение минимум до 524288"
    fi
}

# Функция включает и запускает сервис SonarQube.
enable_and_start_service() {
    # systemd должен перечитать новый unit.
    systemctl daemon-reload

    # enable --now включает автозапуск и запускает сервис сразу.
    systemctl enable --now sonarqube
}

# Функция печатает версии установленных инструментов.
print_installed_versions() {
    # Java 21 подтверждает runtime сервера.
    "${JAVA_HOME_PATH}/bin/java" -version

    # SonarScanner должен быть доступен через стабильный PATH.
    sonar-scanner -v
}

# Главная функция фиксирует порядок системной установки.
main() {
    # Проверяем root до любых системных изменений.
    require_root

    # Проверяем дистрибутив до вызова pacman.
    require_arch_like_system

    # Ставим JDK 21 и базовые утилиты.
    install_system_packages

    # Создаем системную группу.
    ensure_sonarqube_group

    # Создаем системного пользователя.
    ensure_sonarqube_user

    # Готовим временный каталог установки.
    prepare_install_work_dir

    # Скачиваем SonarQube с официального CDN.
    download_if_missing "${SONARQUBE_ZIP_URL}" "${SONARQUBE_ZIP_PATH}"

    # Скачиваем SonarScanner CLI с официального CDN.
    download_if_missing "${SONAR_SCANNER_ZIP_URL}" "${SONAR_SCANNER_ZIP_PATH}"

    # Проверяем ZIP SonarQube до распаковки.
    verify_zip_archive "${SONARQUBE_ZIP_PATH}"

    # Проверяем ZIP SonarScanner до распаковки.
    verify_zip_archive "${SONAR_SCANNER_ZIP_PATH}"

    # Устанавливаем файлы SonarQube.
    install_sonarqube_files

    # Устанавливаем файлы SonarScanner.
    install_sonar_scanner_files

    # Создаем runtime-каталоги.
    prepare_runtime_directories

    # Настраиваем sonar.properties.
    configure_sonarqube_properties

    # Устанавливаем systemd unit.
    install_systemd_service

    # Проверяем системные лимиты до запуска.
    check_runtime_limits

    # Включаем и запускаем сервис.
    enable_and_start_service

    # Печатаем версии для быстрой проверки.
    print_installed_versions

    # Финальное сообщение показывает URL локального интерфейса.
    print_info "SonarQube будет доступен на http://127.0.0.1:9000 после завершения старта"
}

# Запускаем главную функцию с исходными аргументами.
main "$@"
