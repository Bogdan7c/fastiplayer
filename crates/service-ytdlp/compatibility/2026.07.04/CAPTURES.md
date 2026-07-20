# Optional capture procedure

Эта процедура относится только к необязательным user/network captures. Checked-in
official-source synthetic fixtures из `fixtures/official-synthetic/` не содержат
сетевых данных и не требуют этой процедуры.

## До запуска

1. Использовать только `yt-dlp 2026.07.04`.
2. Записать SHA-256 исполняемого файла, `yt-dlp --version`, ОС и UTC-время.
3. Получить явное разрешение владельца URL и учётной записи на capture.
4. Создать отдельный временный каталог вне репозитория с правами только владельца.
5. Не включать third-party plugins. Для hermetic capture передавать
   `--ignore-config --no-plugin-dirs`.
6. Не добавлять `--cookies`, `--cookies-from-browser`, authentication flags или
   user config, если конкретная authorization-сессия не является предметом
   отдельного согласованного capture.

## Exact invocation

Hermetic inventory запускается только с таким app-owned prefix:

```text
yt-dlp --ignore-config --no-plugin-dirs --quiet --no-warnings --simulate --dump-single-json --no-playlist <URL>
```

Hermetic selected-result capture добавляет после `--no-playlist`:

```text
--format <documented-selector> <URL>
```

`--simulate` обязателен: в pinned release он означает «не скачивать видео и
ничего не писать на диск». `--skip-download` не является заменой: он разрешает
запись связанных metadata-файлов.

## Redaction

Capture сначала сохраняется только во временном каталоге и не открывается в
issue, PR, terminal transcript или chat. Затем отдельная копия редактируется:

- каждый input/direct/manifest/fragment/key/page/socket URL заменяется
  структурным `.invalid` URL с сохранением только схемы и роли;
- userinfo, path identifiers, query и fragment удаляются;
- значения всех headers заменяются фиксированными markers;
- `cookies`, cookie-like headers, tokens, signatures, keys, IV и request body
  заменяются фиксированными markers без сохранения длины;
- provider/media/account IDs, title, description и персональные metadata
  заменяются synthetic values;
- Python `repr` с address/process identity заменяется typed marker и получает
  `RequiresLiveExtractorState`;
- сохраняются raw `protocol`, `ext`, `container`, `vcodec`, `acodec`, потому что
  они являются предметом compatibility evidence; значение длиннее profile bound
  не обрезается молча, а отклоняет capture.

После redaction выполняется ручная проверка исходной и sanitized копий бок о бок.
Исходная копия удаляется только владельцем capture после подтверждения, что она
больше не нужна.

## Provenance record

Sanitized fixture обязана содержать:

- уникальный `fixture_id`;
- release, commit и executable SHA-256;
- UTC capture time и OS;
- exact argv с `<redacted-url>` и `<redacted-selector>`;
- режим `hermetic` либо `manual_opt_in`;
- имя extractor-а и result `_type`;
- перечень применённых redactions;
- утверждение, что в checked-in файле нет usable URL/header/cookie/request secret.

Manual opt-in capture хранится отдельно от official-source corpus. Он не
расширяет app guarantee: trusted config/plugin может выполнить собственный код
или I/O, а указанный пользователем cookie jar может быть обновлён самим
system `yt-dlp`.

## Admission в corpus

Перед commit:

1. Поместить sanitized файл в отдельный `fixtures/user-network/` каталог.
2. Добавить его путь и provenance в новую revision manifest-а.
3. Связать evidence с конкретной target либо excluded row.
4. Запустить focused compatibility test.
5. Просмотреть `git diff` и поискать URL, `Cookie`, `Authorization`, token,
   signature, key, user/account/media identifiers.
6. Не переводить provisional/unknown row в Target без отдельного
   profile-extension решения.
