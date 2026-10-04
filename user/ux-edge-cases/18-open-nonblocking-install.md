# Сессия 18. Кнопка Open не подвешивает окно на время установки файла

## Абзац для отправки вместе с этим файлом

> Выполни сессию 18 из приложенного Markdown для Fastiplayer (план `user/ux-edge-cases/`). Сначала прочитай `user/ux-edge-cases/README.md` и `user/ux-edge-cases/results/03.md`. Разрешён только объём, указанный в файле. Соблюдай AGENTS.md. Это изменение жизненного цикла strong-open на UI-потоке: сначала исследование (кто сейчас ждёт блокирующе, кто владеет pending-слотом, как поведут себя параллельные открытия) и предложение архитектуры, затем остановка и согласование со мной простым языком; реализация — после моего решения. Результат запиши в `user/ux-edge-cases/results/18.md`. Работай в main без веток; коммит — только после зелёных проверок и моей ручной приёмки, push не делай.

## Границы

**Зависит от:** 03 (типизированная причина отказа, `StrongMediaOpenUserOutcome`). Если `results/03.md` нет или статус не PASS — сессия заблокирована. Желательно после 04: она вводит владельца уведомлений и сообщение «занято» при повторном Open. Если 04 ещё не сделана, повторный Open во время установки обрабатывается по решению владельца из 03: «занято» молча, только лог.

**Memories:** `mem:app-egui/media-open-coordinator-s10c` (раздел UX03), `mem:app-egui/startup-orchestration-s17`, `mem:app-egui/post-installed-strong-open-compensation-2026-08-23`, `mem:app-egui/queue-replacement-confirmation-s14a`, `mem:app-egui/transport-guard-execution-2026-10-03`, `mem:app-egui/local-open-error-messages-ux02`, `mem:app-egui/app-shell-event-loop-decomposition-s42-2026-08-27`.

**Разрешено:**
- путь кнопки Open и замены очереди: `AppState::load_prepared_local_file` (`state/media_jobs.rs`) и его вызов из обработки `LocalFileOpenResult::Prepared`;
- перевод этого пути на существующий неблокирующий stepwise strong-open (`begin_prepared_media_strong` / `poll_prepared_media_strong`);
- новый маленький владелец «pending установки локального файла из Open», если он нужен;
- место, где кадр/event loop опрашивает этот pending;
- индикатор «Открываем «clip.mkv»…» на время установки (функция `local_open_preparing_message` уже есть);
- тесты.

**Запрещено:**
- менять протокол coordinator-а (фазы, Ready → authorize → Installed barrier, CommitMustFinish), compensation и release accounting;
- менять player-core и `staged_video_preflight_timeout`;
- переводить на неблокирующий путь settings rebuild, suspend/resume, same-item switch, cancel-loop в `strong_media_open.rs`. Их только инвентаризировать (см. часть A);
- трогать жизненный цикл уведомлений (сессия 04).

## Предыстория (факты сессии 03, сверить с кодом)

1. Кнопка Open после подготовки файла вызывает `install_prepared_media_strong` (`state/media_jobs.rs:~92`). Это **блокирующая** обёртка: `drive_media_open_to_terminal` (`state/strong_media_open.rs:~508`) крутит `wait_for_media_open_progress`. Тот на фазе `PlayerStaging` ждёт `MediaInstallReceipt::wait_until_signal_available` — blocking `recv()` без deadline, прямо на UI-потоке.
2. Сверху ожидание ограничено только player-owned `staged_video_preflight_timeout` (15 с по умолчанию). Медленный файл (сеть/NAS, большой probe, тормозной диск) замораживает окно до ~15 с: не перерисовывается, не реагирует на клавиши, KDE может предложить «принудительно завершить».
3. Для старта (S17) и плейлиста уже есть неблокирующий путь: `begin_prepared_media_strong` (`state/strong_media_open/pending/admission.rs`) кладёт транзакцию в `AppState.pending_strong_media_open: Option<PendingStrongMediaOpen>`, а `poll_prepared_media_strong` (`state/strong_media_open/pending.rs:~112`) «никогда не ждёт worker receipt». Его опрашивают `startup_media/pending_install.rs`, `state/playlist_transport.rs`, `state/vod_endpoint_recovery.rs`, `same_item_candidate_switch/lifecycle_bridge.rs`.
4. Pending-слот в `AppState` **один** на всех. Главный архитектурный вопрос: кто его владелец, когда Open, startup и плейлист могут захотеть его одновременно, и кто опрашивает результат Open.
5. Прочие блокирующие вызовы (вне объёма реализации):
   - `frame_prepare/settings_runtime_adapter.rs:~203` (пересборка после смены настроек);
   - `playlist_runtime/suspend_resume.rs:~239`;
   - `state/suspended_media_resume.rs:~141`;
   - `state/same_item_candidate_switch.rs:~456`;
   - `state/strong_media_open.rs:~715` (cancel-loop).

## Задание

### Часть A. Исследование (без правок)
1. Подтвердить, что подвисание реально. Воспроизвести тестом с fake-плеером, который отвечает Ready только через N мс: блокирующий путь держит вызывающий поток всё это время.
2. Карта владения `pending_strong_media_open`:
   - кто кладёт и кто забирает;
   - что происходит сейчас при попытке начать второй strong-open, пока первый pending (Busy? typed error?);
   - как startup и плейлист различают «свой» pending (`StartupPendingInstall`, request_id).
3. Где и когда event loop опрашивает pending для старта и плейлиста. Найти правильное место для опроса Open-установки: по образцу `StartupMediaController::poll_pending_install`, без нового «универсального» опросчика в большом файле.
4. Что сейчас делает `load_prepared_local_file` после успешной установки: `record_installed_media_source`, sibling discovery, play from beginning, сброс ошибок. Всё это должно переехать в обработчик `Installed` неблокирующего пути без потери порядка.
5. Взаимодействие с подтверждением замены очереди (S14A) и guard-ом транспорта (`transport-guard-execution`): что видит пользователь, если во время pending Open нажать Next/Stop или снова Open.
6. Для каждого из прочих блокирующих вызовов (п.5 предыстории) — короткий вердикт: может ли он реально висеть долго (preflight до 15 с) или ждёт только быстрых шагов. Это вход для отдельных решений, не для реализации здесь.

### Стоп: согласование с владельцем
Варианты простым языком, например:
- **(A) Open на существующем pending-слоте.** Open использует тот же `pending_strong_media_open` и тот же stepwise-протокол, что старт и плейлист; опрашивает его маленький владелец «установка из Open». Конфликт слотов решается правилом (например, «новое открытие пользователя вытесняет / ждёт / получает „занято“») — правило выбирает владелец.
- **(B) Отдельный слот для Open.** Меньше пересечений с плейлистом, но два параллельных strong-open к одному player-у. Риск нарушить «один актуальный install», нужно доказать, что coordinator это и так запрещает.
- **(C) Минимум: deadline на блокирующем ожидании.** Окно всё равно замирает до deadline, но меньше; после deadline — отмена и сообщение. Дёшево, но лечит симптом, а не причину.

Для каждого — рекомендация, риски, объём. Отдельно спросить:
- что показывать во время установки (строка «Открываем «clip.mkv»…», спиннер или ничего);
- поведение повторного Open во время pending: вытеснить старое открытие или «занято».

### Часть B. Реализация
- Open-путь не вызывает ни `install_prepared_media_strong`, ни `wait_for_media_open_progress`. Закрепить source-scan тестом по образцу `startup_orchestration_uses_only_stepwise_strong_install_boundary`.
- Тексты ошибок и молчание для отмены/«занято» — через `StrongMediaOpenError::user_outcome()` и `local_open_failure_message` (сессия 03), без новой классификации.
- Старое воспроизведение при отказе до barrier-а не трогается; post-barrier — существующая compensation.
- Различия Installed / PlayerFailed / PlayerRejected / Cancelled / Busy / FatalInvariant сохраняются.

## Тесты
- **Fake-плеер отвечает с задержкой.** Вызов Open возвращается сразу (без ожидания receipt); последующие опросы доводят до `Installed`, и файл реально становится текущим media.
- **Реальный путь.** Настоящий `PlayerWorker` + демуксер, который отдаёт `TemporarilyUnavailable` (медленный preflight). Опрос Open не блокирует; по истечении preflight-таймаута (уменьшенного через `PlayerTickConfig` в тесте) пользователь видит «файл читается слишком долго», старое media на месте.
- Отказ (неизвестный кодек) через неблокирующий путь → тот же текст, что в сессии 03.
- Повторный Open во время pending → поведение, выбранное владельцем; ни одна установка не теряется молча, terminal забирается ровно один раз.
- Open во время pending установки плейлиста/старта, и наоборот → выбранное правило, без двух одновременных strong-open.
- Source-scan: Open-путь не содержит блокирующих вызовов.

## Ручная приёмка
1. Сделать «медленный» файл: например, положить большой файл на медленный носитель/NAS. Если такого нет — проверить на сборке с маленьким `staged_video_preflight_timeout` и демуксером-заглушкой (сценарий согласовать в части A).
2. Нажать Open → выбрать файл → окно остаётся отзывчивым: перерисовывается, двигается, реагирует на пробел/громкость; видна строка «Открываем «…»…».
3. Нормальный файл через Open открывается так же быстро, как раньше; после открытия работают sibling discovery и автозапуск.
4. Повторный Open во время установки → поведение по решению владельца.

## Выходной барьер
`results/18.md` по общему шаблону. Обновить `mem:app-egui/media-open-coordinator-s10c` (кто теперь блокирующие вызовы, кто опрашивает Open) и `mem:app-egui/local-open-error-messages-ux02`, если меняется путь Open.
