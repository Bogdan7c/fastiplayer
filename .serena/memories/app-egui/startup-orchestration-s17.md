# Session 17 — startup restore и CLI priority/fallback (2026-07-16)

## Владение и порядок
- `AppShell` остаётся thin lifecycle owner: удерживает acquired `AppInstanceLease`, запускает state inspection и CLI preparation только после lease/config bootstrap, drain-ит wake/poll и завершает startup/state/player owners до release lease.
- `StartupMediaController` + `startup_media::orchestration` владеют process-lifetime startup winner/fallback policy и фазами `WaitingForRuntime | Preparing | PreparedAwaitingAllocator | Applying | Activated | Idle | Failed | Shutdown`. Prepared CLI/local/YouTube/direct media остаётся ID-less до typed allocator gate.
- `PlaylistStartupOwner` единолично решает valid/missing/quarantine/protected lineage. Valid state всегда передаёт persisted allocator watermark даже после D65 supersede. Missing и successful quarantine создают persistent initial lineage. Newer/unrecognized/quarantine-failed state создаёт отдельную non-persistent runtime generation с blocked writer; protected IDs не merge-ятся и не записываются.
- Renderer-bound stepwise strong install принадлежит `AppState` в `state/strong_media_open/pending.rs`. Startup poll не вызывает blocking `install_prepared_media_strong`: begin быстро выполняет staging/admission, poll неблокирующе проходит coordinator phases, Ready authorization, exact Installed, optional exact position restore, `PlaybackIntentUpdateReceipt::try_outcome`, затем lineage/domain commit. Между poll-вызовами pending receipt обязан сохранить текущую реальную фазу; transient `Polling` marker возвращает waiting phase только если вложенный шаг не установил successor. Это предотвращает повторный вход в coordinator protocol после exactly-once consumption Installed terminal. Для `SeekTo` player receipt остаётся pending до final seek commit, поэтому startup Pause не может отменить незавершённый SeekLanding; подробности: `mem:player-core/installed-position-restore-receipt-2026-07-19`. Старый blocking wrapper остаётся только для non-startup settings/local callers.

## Winner, fallback и D65
- Valid restored queue при CLI остаётся unopened fallback. CLI success заменяет её только после exact `EnqueuedAtPlayerOwner -> Installed`; pre-barrier preparation/cancel-win/rejection сохраняют fallback. Missing resolution/terminal, fatal invariant и post-barrier failure sticky fatal и никогда не активируют fallback.
- No CLI + restored `Some(current)` готовится без sibling scan. Exact original current может получить `StartupPosition::Restore` из отдельного resume sidecar; fallback/Skip targets всегда `KeepStart`, и все attempts завершаются `StartPaused`. Restored `current=None` остаётся idle без implicit selection. `controller/startup_restore.rs` использует bounded committed-ID automatic plan для Skip; Stop и RepeatOne останавливают; D70 unavailable rows получают runtime badge без delete/dirty. Полный position contract: `mem:playlist/resume-position-sidecar-2026-07-19`.
- `StartupMutationDraft` действует до gate; `playlist_runtime/startup_retained.rs` продолжает те же bounded latest-wins semantics вокруг post-gate install linearization. Clear/replacement supersede earlier queue action, prepared Adds bounded cap, repeat/shuffle coalesce. Cancel-win применяет retained winner exactly once; enqueue-win сначала полностью публикует unavoidable Installed identity/domain commit, затем retained action. IDs/dirty не появляются до разрешённого domain boundary.
- CLI local использует target-first open и запускает sibling discovery только после exact Installed target. CLI URL — one-item replacement. Native non-UTF-8 CLI media остаётся exact `PathBuf`; URL не строится lossy.
- Trusted sensitive direct CLI URL не получает второй acknowledgement dialog. Service-owned locator сохраняет reopenable identity, а отдельный redacted informational warning является process-lifetime read-only status и не gate.

## Wake, shutdown и проверки
- Startup preparation, prepared-awaiting-allocator и stepwise Applying считаются pending background work. Wake/defensive poll продвигают транзакцию без блокировки UI; `ControlFlow::Wait` сохраняется.
- Shutdown закрывает admission, drop-ит prepared ownership, cancel/drain-ит pending strong transaction по authoritative cancel/enqueue semantics и не разрешает поздний install/quarantine/write поверх terminal lifecycle. Session 14B suspend checkpoint и process restart zero+Pause не менялись.
- Focused tests находятся в `playlist_runtime/startup/tests.rs`, `playlist_runtime/startup_retained.rs`, `startup_media/tests.rs` (controller/CLI routing/shutdown), `startup_media/pending_install.rs`, `startup_media/orchestration/pending_work_tests.rs`, `state/strong_media_open/pending.rs`.
- PASS: 516 app-egui no-default, 75 playlist-core, 33 playlist-state, 52 playlist-discovery, 9 direct (+1 manual ignored), 33 YouTube (+4 manual ignored), app strict Clippy no-default/all-features, fmt, Rust 1.96 locked workspace check, guardrails, diff check и Serena diagnostics.
- Полный handoff: `user/playlist_queue_implementation_plan.md`. Следующий разрешённый scope — Session 18 UI; Session 17 UI не добавляла.


## S17S startup/desktop playlist routing (2026-07-21)
- Positional CLI/desktop path с case-insensitive extension `.m3u`, `.m3u8`, `.xspf` или `.cue` классифицируется как `InitialMedia::Playlist` после URL classification; ordinary local media, direct URL и yt-dlp routing не изменены. В repository нет отдельного desktop/AppStream manifest: OS association входит через тот же positional argument.
- `PlaylistRuntime::start_startup_playlist_import` запускает существующий authoritative `PlaylistImportIoOwner` с `PlaylistImportIntent::StartupReplace`. Parsed ID-less draft ждёт canonical allocator/load gate; preview/capacity/partial decision остаются S08 transaction owner-у. StartupReplace bypass-ит replacement/sensitive confirmation, поэтому после explicit partial Continue второго confirmation нет.
- Только successful queue commit публикует `StartupPlaylistCommitReceipt { first_item_id, expected_queue_revision }`. `plan_startup_playlist_first_install` повторно валидирует exact queue revision и source-order first Item ID, затем строит один `ReservedQueueMutation::select_committed` plan. Никакого sibling/next scan при первом failure нет; synchronous source/strong rejection помечает только exact first Item failed.
- Startup controller сохраняет прежние CLI winner, restored fallback, generation/supersede, retained-action и stepwise strong-open gates. Manual row/group Play и interactive import supersede-ят незавершённый startup playlist flow. CUE first target сохраняет `MediaPlaybackWindow`.
- Focused tests: все четыре формата, empty/partial/capacity, CUE window, structural competition, exact first failure/no scan, commit-before-open и exact Item/Group allocator accounting. Verification: 805 app tests с default и no-default features, Rust 1.96 locked workspace check, fmt, refactor guardrails, diff check, touched-file diagnostics и strict Clippy кроме двух известных pre-existing `large_enum_variant` baseline warnings.


## UX06: политика фатальных ошибок запуска (2026-10-05)

- Владелец — `crates/app-egui/src/fatal_startup/`. Состав:
  - тип `FatalStartupError { reason: FatalStartupReason, technical_detail }`: детали только в лог, пользователю только человеческий текст из `messages.rs`;
  - перевод `ProcessBootstrapError` в `bootstrap_mapping.rs`;
  - путь к папке настроек с `~`, плюс shell-quoted аргумент для `sudo chown -R "$USER": …`, в `config_location.rs`.
- Порядок в `main`: tracing инициализируется ПЕРВЫМ (до bootstrap) → `run_application() -> Result<(), FatalStartupError>` → `conclude_process(outcome, &SystemFatalStartupPresenter).exit_code()`. Штатно 0, фатальная ошибка 1 (код 70 shutdown-timeout отдельный).
- `ProcessBootstrapError::Lease { lease_error, config_dir }` и `LoadConfig { config_error: Box<ConfigError>, config_dir }` несут папку настроек для текста. В Display (лог) путь по-прежнему не попадает. Box нужен из-за clippy `result_large_err`.
- Решения владельца:
  - показ: kdialog на KDE (`XDG_CURRENT_DESKTOP` содержит `KDE`), иначе первым zenity; вторая утилита запасная, затем критическое D-Bus уведомление; текст ВСЕГДА дублируется в stderr;
  - окно показывается и при запуске из терминала;
  - «уже запущен» показывается окном (до сессии 13 forwarding);
  - в ошибке папки настроек есть путь и команда `chown`.
- Утилита считается сработавшей только при exit 0. Ненулевой код или отсутствие программы ведут к следующему каналу. У zenity обязателен `--no-markup`.
- D-Bus уведомление: `desktop_integration::send_critical_desktop_notification`. Блокирующий zbus, urgency=2, expire=0, body экранируется, таймаут ответа 5 с.
- Тесты:
  - `fatal_startup/tests.rs` — сквозные: fake-presenter + код выхода для нет GPU / окна / сеанса / lease mismatch / bad CLI / нормального запуска;
  - `messages/tests.rs` — нет Debug-имён, тексты уникальны;
  - `dialog_utility/tests.rs` — поддельная утилита-скрипт проверяет argv и exit codes;
  - `presentation/tests.rs` — цепочка каналов, порядок по desktop;
  - `app_instance` — `bootstrap_failures_reach_user_as_human_text_with_nonzero_exit`;
  - `desktop-integration/src/notification/linux/tests.rs` — частный dbus-daemon с конфигом БЕЗ servicedirs (иначе auto-activation настоящей службы на 60 с).

## UX13: несколько файлов в CLI и второй запуск (2026-10-08)

- Грамматика `ProcessArgs`: ноль и больше позиционных (`Vec<OsString>`, порядок и байты сохраняются), `ExtraPositional` удалён. `take_initial_media_arguments` / `initial_media_arguments`.
- Классификация `startup_media/initial_arguments.rs`: 0/1 аргумент — прежний путь (`resolve_initial_media_argument`); ≥2 — правила броска сессии 12 (есть локальные файлы → только они; иначе первый плейлист; иначе первая ссылка; остальное — info-уведомление `startup_arguments_message.rs`). Вариант `InitialMedia::Several(SeveralInitialArguments)` несёт победителя и уведомление; `startup_media/cli_local_files.rs` раскладывает его в начале `start_pending_initial_media`.
- Поток: первый файл — обычный CLI single-file путь (fallback, allocator gate, stepwise install); `StartupSiblingDiscovery::AppendCliFollowUpFiles(Vec<PathBuf>)` — после exact Installed остальные файлы добавляются `PlaylistRuntime::append_local_files_after_startup_target` (`playlist_runtime/startup_follow_up_files.rs`) до retained actions; superseded → ничего; без sibling discovery и без подтверждения.
- Lease занят → `bootstrap_with` вызывает пересылку в запущенный экземпляр, config не читается; `ProcessStart::ForwardedToRunningInstance` → exit 0. Окно «уже запущен» (UX06) осталось только для провала пересылки без файлов. Граница и протокол: `mem:app-egui/instance-forwarding-ux13`.
- Тесты: `app_instance/tests.rs` (вынесены из mod.rs ради лимита 800), `startup_media/initial_arguments/tests.rs`, `startup_media/cli_local_files/tests.rs`, `playlist_runtime/startup_follow_up_files/tests.rs`.

## S23 yt-dlp startup integration (2026-07-22)

- CLI/restored yt-dlp preparation now uses the single S19 -> S21C -> S22 app composition path; the old service-owned WebM opener no longer exists. Startup winner/fallback, allocator gate and exact Installed ordering are unchanged.
- `YtDlpStartupJob` propagates both its atomic lifecycle callback to the cancellable extractor and a shared `source_core::CancellationToken` to transport/demux; shutdown cancels both before bounded join.
- Current details and limitations: `mem:app-egui/queue-owned-web-open-s23-2026-07-22`.
