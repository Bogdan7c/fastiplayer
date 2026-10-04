# Сессия 03. Причина отказа установки media доходит до пользователя

## Абзац для отправки вместе с этим файлом

> Выполни сессию 03 из приложенного Markdown для Fastiplayer (план `user/ux-edge-cases/`). Сначала прочитай `user/ux-edge-cases/README.md` и `user/ux-edge-cases/results/02.md`. Разрешён только объём, указанный в файле. Соблюдай AGENTS.md. Это изменение boundary player-core → app: сначала исследование (откуда player отказывает, какие причины теряются) и предложение формы типизированной причины, затем остановка и согласование со мной простым языком; реализация — после моего решения. Результат запиши в `user/ux-edge-cases/results/03.md`. Работай в main без веток; коммит — только после зелёных проверок и моей ручной приёмки, push не делай.

## Границы

**Зависит от:** 02 (единая функция «причина → текст» и формат с именем файла). Если `results/02.md` нет или статус не PASS — сессия заблокирована.

**Memories:** `mem:player-core/core`, `mem:player-core/video-requirement-preflight`, `mem:player-core/staged-media-settings-reconfigure-fence-2026-08-05`, `mem:player-core/staged-position-gate-slice-c-2026-07-26`, `mem:app-egui/media-open-coordinator-s10c`, `mem:app-egui/post-installed-strong-open-compensation-2026-08-23`.

**Разрешено:** terminal-исходы media-open (`app-egui/src/media_open/types.rs:~528` — `PlayerRejected`/`PlayerFailed`), их источник в player-core (событие/ответ на staged install), `StrongMediaOpenError::Terminal` (`app-egui/src/state/strong_media_open.rs:~735`), `load_prepared_local_file` (`app-egui/src/state/media_jobs.rs:70-80`), сообщения через функцию из сессии 02; тесты.

**Запрещено:** менять семантику staged install / compensation / release accounting, решения preflight (что поддерживается), выбор backend и fallback-политику; жизненный цикл overlay (сессия 04); web-причины (сессия 08 — но форма причины должна быть пригодна и для неё).

## Предыстория (факты аудита, сверить с кодом)

1. `PlayerRejected { request_id }` / `PlayerFailed { request_id }` **не несут причины**. `StrongMediaOpenError::Terminal` получает `{:?}`-дамп исхода.
2. `load_prepared_local_file` оборачивает любую ошибку установки в «Ошибка открытия media-файла {label}: worker недоступен: {error}» (`media_jobs.rs:78`) — ложь: worker доступен, он отказал (кодек не поддерживается, нет подходящего video backend, preflight не прошёл).
3. Пользователь видит английский Debug вида «media-open returned an unexpected terminal outcome: PlayerRejected { request_id: … }»; настоящая причина — только в логе (если вообще).
4. `drive_media_open_to_terminal` (`strong_media_open.rs:~520`) ждёт terminal на UI-потоке через `wait_until_signal_available` (`media_open/coordinator.rs:390-406`) — проверить в исследовании, ограничено ли ожидание по времени (подозрение на подвисание окна при медленном preflight). Только зафиксировать; исправление — отдельным решением.

## Задание

### Часть A. Исследование (без правок)
1. Найти все места player-core, где staged install отклоняется или падает, и какую информацию они имеют (тип ошибки, кодек, backend, причина preflight).
2. Найти, где эта информация теряется на пути worker → coordinator → app.
3. Предложить форму причины: enum в player-core (например «кодек не поддерживается», «нет подходящего видео-декодера», «аудиоустройство недоступно», «внутренняя ошибка») + технические детали только для лога. Оценить, не тянет ли это знание app-уровня в player-core.
4. Ответить на п.4 предыстории (есть ли таймаут ожидания).

### Стоп: согласование с владельцем
Варианты простым языком, например: (A) типизированная причина в terminal-исходе; (B) причина передаётся отдельным событием и сопоставляется по `request_id`; (C) только убрать ложный текст и показать общий «формат не поддерживается». Рекомендация, риски, объём.

### Часть B. Реализация
- Причина типизирована на boundary (без строк с неочевидным смыслом), app маппит её в текст функцией из сессии 02.
- Старые различия Rejected / Failed / Busy / Stale / Cancelled сохраняются.

## Тесты
- fake player отклоняет install с каждой причиной → пользователь видит соответствующий текст с именем файла, без «worker недоступен», без `{:?}`;
- реальный путь: файл с неподдерживаемым кодеком (фикстура из `mem:testing/media-fixtures`, если есть подходящая; иначе сгенерировать) → понятная причина;
- предыдущее воспроизведение продолжается после отказа (регрессия compensation);
- Busy/Stale/Cancelled не превращаются в ошибку для пользователя.

## Ручная приёмка
Открыть файл с неподдерживаемым кодеком и (если возможно) с `preferred_backend=hardware` без поддержки кодека железом → понятное сообщение; старое видео продолжает играть.

## Выходной барьер
`results/03.md` по общему шаблону. Обновить `mem:player-core/core` и `mem:app-egui/media-open-coordinator-s10c` (новая форма terminal-причины).
