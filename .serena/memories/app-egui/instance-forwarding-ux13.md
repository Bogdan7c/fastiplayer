# UX13: второй запуск → файлы в первый экземпляр (IPC-граница, 2026-10-08)

План/решения владельца: `user/ux-edge-cases/13-single-instance-forwarding.md` (решения 1–5), отчёт `user/ux-edge-cases/results/13.md`.

## Решения владельца
- Нейтральный контракт + транспорт под ОС; Linux = D-Bus `org.freedesktop.Application` (свой Unix-сокет отклонён). Windows/macOS добавляют только транспорт.
- App ID `io.github.Bogdan7c.Fastiplayer` (`desktop_integration::FASTIPLAYER_APPLICATION_ID`), объект `/io/github/Bogdan7c/Fastiplayer`. Менять нельзя без миграции.
- Окно на Wayland поднимается билетом второго запуска через additive патч winit (`mem:dependency-patches/core`).
- Несколько файлов в CLI разрешены (и при первом запуске, `mem:app-egui/startup-orchestration-s17` UX13).

## Транспорт (`crates/desktop-integration/src/instance_forwarding/`)
- `mod.rs` — нейтральные типы: `ForwardedInstanceRequest {Activate | Open(Vec<ForwardedUri>)} + Option<WindowActivationToken>` (Debug redacted), `ForwardedRequestSink` (неблокирующий), `ForwardedRequestDelivery` + `ForwardedRequestAcknowledgement` (Drop без acknowledge → отправителю «завершается»), `start_instance_forwarding_service`, `forward_to_running_instance`, типизированные `InstanceForwardingServiceError` / `InstanceForwardingError {SessionBusUnavailable, InstanceNotListening, InstanceNotResponding, InstanceShuttingDown, ForeignInstanceOwner, Rejected, Transport, UnsupportedPlatform}`.
- `admission.rs` — единые правила для отправителя и получателя: ≤10 000 URI, URI ≤16 KiB, всего ≤4 MiB, только строки со схемой RFC 3986, без управляющих символов; пустой Open = Activate; билет — печатный ASCII ≤1024; `admit_peer`: только uid == свой (root тоже отклоняется), неизвестный uid отклоняется.
- `linux/service.rs` — свой connection, объект регистрируется до `RequestName(DoNotQueue)` (NameTaken → `BusNameUnavailable`). Обработчики **async**: uid отправителя через `GetConnectionUnixUser`, проверка, доставка в sink, ожидание подтверждения UI через `async_channel` + `async_io::Timer` (блокировка executor-а zbus заклинила бы запрос uid). Ошибки D-Bus: `io.github.Bogdan7c.Fastiplayer.Error.{Rejected,NotResponding,ShuttingDown}`. `ActivateAction` → Rejected. `platform_data`: `activation-token` (Wayland) или `desktop-startup-id` (X11).
- `linux/client.rs` — ждёт `NameHasOwner` (первый мог взять lease, но ещё не стартовать службу), сверяет uid владельца имени, вызывает через **низкоуровневый `Connection::call_method`**: в zbus 5.15 `#[proxy]`-вызовы игнорируют `method_timeout` (поймано тестом зависшего экземпляра). NoReply → ShuttingDown, TimedOut → NotResponding.

## Приложение
- Отправитель `app_instance/forwarding.rs`: `bootstrap_with` при `AlreadyRunning` вызывает пересылку с нетронутыми аргументами; config второго процесса не читается. Аргумент → URI: ссылка, если `classify_startup_url` не `NotUrl` (как первый запуск), иначе путь, привязанный к cwd второго процесса, → `external_open::uri::file_uri_from_absolute_path` (байты, round-trip с `item_from_uri`). Билет из `XDG_ACTIVATION_TOKEN`, затем `DESKTOP_STARTUP_ID`. Таймауты: ожидание приёмника 10 с, вызов 8 с (> 5 с подтверждения получателя). Итог `ProcessStart::{Primary(Box<…>), ForwardedToRunningInstance}` → exit 0. Ошибки → `FatalStartupReason::{RunningInstanceNotResponding, RunningInstanceShuttingDown, RunningInstanceDidNotTakeFiles, AlreadyRunning(без файлов)}`.
- Получатель `crates/app-egui/src/instance_forwarding/`: `InstanceForwardingInbox` (process-lifetime поле `AppShell`, старт в `AppShell::new` после последней fallible операции; shutdown первым в `finish_process_shutdown`). Ящик `sync_channel(8)` + `AppWakeOwner::InstanceForwarding`; подтверждение при `drain_deliveries` на UI-потоке; до готовности окна запросы ждут (≤8, старые отбрасываются), исполняет `AppShell::execute_pending_forwarded_requests` (в т.ч. в конце `restore_runtime`). `external_open_request_for` → `ExternalOpenRequest { Video, item_from_uri… }` → `AppState::handle_forwarded_open_request` → диспетчер сессии 12 (`mem:app-egui/external-open-drag-drop-ux12`). `window_activation.rs` — единственный winit-aware файл: билет → `activate_with_token`, иначе/NotSupported → `focus_window` + `request_user_attention`.

## Ручная приёмка (2026-10-08, агент по поручению владельца)
- PASS: Dolphin «Открыть с помощью» (1 файл / 4 файла) на KDE Wayland — окно поверх Dolphin по билету; X11 — через `focus_window`; без аргументов — `AttentionRequested`; первый под `SIGSTOP` → kdialog «не отвечает» через 8 с.
- Запрос, отправленный замороженному экземпляру, после `SIGCONT` не исполняется задним числом: клиент закрывает соединение сразу после ответа/таймаута, и `GetConnectionUnixUser` для ушедшего отправителя падает → `UnidentifiedPeer`. Это опирается на то, что соединение клиента живёт только на время вызова (`forward_with`); не продлевать его.
- Приём: временный `.desktop` в `~/.local/share/applications` + `kbuildsycoca6`, изолированный `XDG_CONFIG_HOME`; свежий config имеет `player.start_paused = true`, поэтому открытый файл стартует на паузе.

## Ограничения
- Пересланные не-http(s) ссылки (ftp, rtmp…) получают уведомление «не поддерживается», как при броске; первый запуск их открывает.
- `DBusActivatable`/`.desktop` в репозитории нет; MPRIS `Raise` по-прежнему заглушка.
- Пересылка во время открытия файла → «Файл ещё открывается» (правило броска).

## Тесты
`desktop-integration/src/instance_forwarding/{admission/tests.rs, linux/tests.rs}` (частный dbus-daemon без servicedir: Open/Activate доходят, зависший UI, зависший процесс целиком, ShuttingDown, Full, нет приёмника, поздний старт первого, release имени, занятое имя, мусор/лимиты/сигнатура/ActivateAction, билеты), `app-egui/src/app_instance/{tests.rs, forwarding/tests.rs}`, `instance_forwarding/tests.rs` (ящик, порядок, Full/Closed, сквозной «второй запуск → первый» через настоящий диспетчер), `external_open/uri/tests.rs` (round-trip).
