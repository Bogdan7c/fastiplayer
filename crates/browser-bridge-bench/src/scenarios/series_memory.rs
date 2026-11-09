//! Сценарии `series-memory-*` (сессия 14b2): память выбора зрителя — озвучка, качество, субтитры —
//! по сайту и сериалу. Экстеншен запоминает выбор в `chrome.storage.local` и применяет его один раз
//! на серию; в приватных окнах память только читается и не пишется ни байта.
//!
//! Страница стенда `series-variants` играет роль сайта. Зритель «нажимает» озвучку на самой странице
//! (параметр `pick_dub=dub-b@1500`: после команды робота `hook:pick-dub` страница ждёт 1,5 с и жмёт
//! кнопку «Dub B» тем же обработчиком, что и мышь). Робот плеера и меню плеера в этих сценариях
//! выбор НЕ делают: тестовая переменная `PlayerTestVariables::with_page_variant_choices` действует
//! на всё окружение, и с ней проверки памяти были бы пустыми (меню само «выбирало» бы то же самое).
//!
//! # Сценарий `series-memory-restart` (Chromium и Firefox)
//! 1. Сеанс 1: зритель выбрал Dub B на странице → страница пишет `generation cause=dub … dub=dub-b`,
//!    память содержит РОВНО одну запись под отпечатком «сайт + сериал» с `audio = "Dub B"`.
//! 2. Браузер закрыт, окно плеера ушло. Сеанс 2 на том же профиле, страница сама ничего не жмёт:
//!    страница сама оказывается на Dub B (нажал экстеншен по памяти), кадры растут в НОВОМ сеансе.
//!    Тут же «забыть» (то же сообщение, что у кнопки настроек): в памяти нет ни одного сериала.
//! 3. Сеанс 3 на том же профиле: за окно [`QUIET_WINDOW`] после передачи озвучку никто не меняет,
//!    страница остаётся на Dub A, память по-прежнему пуста, кадры растут. Это контроль: без него
//!    «Dub B пришёл сам» мог бы быть свойством страницы или стенда, а не памяти.
//!
//! # Сценарий `series-memory-incognito` (Chromium и Firefox)
//! Оба сеанса идут в приватном окне, куда экстеншен пущен явно (иначе он вообще не работал бы
//! и проверка была бы пустой; приговор требует, чтобы экстеншен видел вкладку стенда как приватную).
//! а) зритель выбрал Dub B на странице → страница переключилась, а в памяти после паузы НЕТ записи
//!    (не пишется даже `used_at`);
//! б) стенд подсаживает запись (`audio = "Dub B"`, `used_at` из прошлого) → новый приватный сеанс
//!    без выбора → страница сама оказывается на Dub B (чтение работает), а запись после сеанса
//!    ровно та, какой была: `used_at` не изменился (записи не было).
//!
//! Эти сценарии требуют готовой функции памяти выбора в экстеншене. Без неё они честно падают на
//! «страница сама не перешла на Dub B».
//!
//! # Что проверено на живых браузерах (спайк 14b2)
//! - **Перезапуск.** Память переживает перезапуск браузера на том же профиле и в Chromium, и в
//!   Firefox. Для Firefox отдельных настроек не нужно: стенд ставит экстеншен ВРЕМЕННО, но Firefox 159
//!   `storage.local` временного экстеншена при выходе не стирает (проверено прогоном без
//!   `extensions.webextensions.keepStorageOnUninstall`; UUID экстеншена закреплён в `user.js`).
//! - **Chromium, приватное окно.** `--incognito` даёт окно инкогнито, а экстеншен пускают туда только через
//!   `Extensions.loadUnpacked { enableInIncognito: true }` (нужен `--enable-unsafe-extension-debugging`;
//!   `--load-extension` в инкогнито не пускает). `PUT /json/new` и `Target.createTarget` вкладку в
//!   это окно НЕ кладут (открывают обычное окно, `tab.incognito = false`), а контекст
//!   `Target.createBrowserContext` блокирует экстеншен (`ERR_BLOCKED_BY_CLIENT`). Рабочий путь:
//!   вкладку открывает сам экстеншен (`tabs.create({ windowId })` из страницы options.html), см.
//!   `chromium::new_private_target`. Служебные страницы робота (передача, чтение памяти) тоже
//!   открываются в окне инкогнито.
//! - **Firefox, приватное окно.** `Addon:Install { allowPrivateBrowsing: true }` и pref
//!   `browser.privatebrowsing.autostart = true` (все окна приватные): `tab.incognito === true`,
//!   передача и плеер работают как обычно.
//!
//! Приговор `series-memory-incognito` требует `tab.incognito === true` у вкладки стенда в обоих сеансах.

mod judge;
mod session;
mod storage;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use self::judge::{
    IncognitoEvidence, PICKED_DUB, PICKED_DUB_LABEL, RestartEvidence, judge_incognito,
    judge_restart,
};
use self::session::{
    GENERATION_LINE_PREFIX, PagePick, SessionPlan, begin_session, end_session, wait_for_grant,
};
use self::storage::{
    BENCH_SERIES_KEY, ForgetReply, STORAGE_KEY, StoredMemory, StoredSeriesChoice,
    choice_fingerprint, forget_script, parse_forget_reply, parse_memory, parse_tabs, read_script,
    seed_script, site_key_of_origin, tabs_script,
};
use super::capture::MIN_PRESENTED_FRAMES;
use super::context::{ScenarioError, Stand};
use super::handoff_run::{ACK_TIMEOUT, HandoffRun};
use super::outcome::{StepResult, Verdict};
use super::page_filter::PageFilter;
use super::player_commands::FRAMES_TIMEOUT;
use super::series::SERIES_PAGE;
use crate::browsers::{BrowserKind, WindowPrivacy};
use crate::environment::BenchEnvironment;

/// Выбор зрителя на странице: Dub B через 1,5 с после команды робота.
const PAGE_PICK: PagePick = PagePick {
    dub_id: PICKED_DUB,
    delay_ms: 1_500,
};
/// Имя хука страницы (`hooks` в `series-variants.html`) и его подтверждение в журнале страницы.
const PICK_COMMAND: &str = "hook:pick-dub";
/// Сколько ждём, пока экстеншен запишет выбор (запись асинхронна).
const MEMORY_WRITE_TIMEOUT: Duration = Duration::from_secs(15);
/// Шаг опроса памяти.
const MEMORY_POLL: Duration = Duration::from_millis(500);
/// Сколько ждём, пока экстеншен сам применит память после передачи.
const APPLY_TIMEOUT: Duration = Duration::from_secs(40);
/// Окно наблюдения контрольного сеанса: за столько секунд после передачи экстеншен, будь у него
/// память, УЖЕ нажал бы озвучку (приговор требует, чтобы сеанс 2 уложился в это же окно).
const QUIET_WINDOW: Duration = Duration::from_secs(15);
/// Пауза, за которую неверная запись в приватном окне точно успела бы появиться (запись в память
/// занимает доли секунды; выбор на странице обрабатывается сразу).
const PRIVATE_SETTLE: Duration = Duration::from_secs(6);
/// Насколько кадры должны вырасти между двумя сводками.
const MIN_FRAMES_GAINED: u64 = 30;
/// Сколько ждём роста кадров (сводки раз в 10 с).
const GROWTH_TIMEOUT: Duration = Duration::from_secs(45);
/// Давность подсаженной записи: заметно в прошлом (чтобы «обновили `used_at`» отличалось от «не тронули»),
/// но не настолько, чтобы экстеншен выбросил её как просроченную.
const SEEDED_AGE: Duration = Duration::from_secs(3_600);

/// Идентификаторы страниц команды (для проверки `--page` в командной строке).
pub(super) fn page_ids() -> Vec<&'static str> {
    vec![SERIES_PAGE]
}

/// Оба сценария, каждый в свежем окружении.
pub(super) fn run_all(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    if !filter.admits(SERIES_PAGE) {
        return Vec::new();
    }
    let started = Instant::now();
    let restart = stand.with_environment(|environment| drive_restart(stand, environment, browser));
    let restart_elapsed = started.elapsed();
    let started = Instant::now();
    let incognito =
        stand.with_environment(|environment| drive_incognito(stand, environment, browser));
    vec![
        StepResult::new(
            "series-memory-restart",
            browser,
            SERIES_PAGE,
            restart,
            restart_elapsed,
        ),
        StepResult::new(
            "series-memory-incognito",
            browser,
            SERIES_PAGE,
            incognito,
            started.elapsed(),
        ),
    ]
}

/// Отпечаток пары «сайт + сериал» этого прогона: сайт — хост сервера стенда.
fn bench_fingerprint(stand: &Stand) -> String {
    choice_fingerprint(
        &site_key_of_origin(&stand.site.main_origin()),
        BENCH_SERIES_KEY,
    )
}

/// Читает память выбора через открытую служебную страницу экстеншена (вкладку не переключает).
fn read_memory(run: &mut HandoffRun<'_>) -> Result<StoredMemory, String> {
    let raw = run
        .browser
        .run_in_service_page(&read_script())
        .map_err(|error| format!("read {STORAGE_KEY}: {error}"))?;
    parse_memory(&raw)
}

/// Ждёт, пока в памяти появится хотя бы один сериал; возвращает последнее прочитанное в любом случае
/// (по таймауту приговор покажет, что там было).
fn read_memory_when_written(
    run: &mut HandoffRun<'_>,
    failed_steps: &mut Vec<String>,
) -> Option<StoredMemory> {
    let deadline = Instant::now() + MEMORY_WRITE_TIMEOUT;
    loop {
        match read_memory(run) {
            Ok(memory) if memory.series_count() > 0 || Instant::now() >= deadline => {
                return Some(memory);
            }
            Ok(_) => std::thread::sleep(MEMORY_POLL),
            Err(error) => {
                failed_steps.push(format!("read-memory: {error}"));
                return None;
            }
        }
    }
}

/// Читает память один раз; ошибка чтения — провал шага.
fn read_memory_once(
    run: &mut HandoffRun<'_>,
    failed_steps: &mut Vec<String>,
) -> Option<StoredMemory> {
    match read_memory(run) {
        Ok(memory) => Some(memory),
        Err(error) => {
            failed_steps.push(format!("read-memory: {error}"));
            None
        }
    }
}

/// Ждёт кадры плеера: первые после передачи (`first`) или рост между сводками.
fn wait_for_first_frames(run: &mut HandoffRun<'_>, failed_steps: &mut Vec<String>) -> bool {
    match run
        .player_log
        .wait_for_presented_frames(MIN_PRESENTED_FRAMES, FRAMES_TIMEOUT)
    {
        Ok(_) => true,
        Err(error) => {
            failed_steps.push(format!("player-presents-frames: {error}"));
            false
        }
    }
}

fn wait_for_frame_growth(run: &mut HandoffRun<'_>, step: &str, failed_steps: &mut Vec<String>) {
    if let Err(error) = run
        .player_log
        .wait_for_frame_growth(MIN_FRAMES_GAINED, GROWTH_TIMEOUT)
    {
        failed_steps.push(format!("{step}: {error}"));
    }
}

/// Открывает служебную страницу экстеншена; провал — шаг.
fn open_service_page(run: &mut HandoffRun<'_>, failed_steps: &mut Vec<String>) -> bool {
    match run.open_service_page() {
        Ok(()) => true,
        Err(error) => {
            failed_steps.push(format!("open-service-page: {error}"));
            false
        }
    }
}

/// Зритель нажимает озвучку на странице и ждёт, пока страница переключилась.
fn viewer_picks_dub_on_page(run: &mut HandoffRun<'_>, failed_steps: &mut Vec<String>) -> bool {
    run.tell_page(PICK_COMMAND);
    if let Err(error) = run.wait_ack(PICK_COMMAND, ACK_TIMEOUT) {
        failed_steps.push(format!("page-picks-dub: {error}"));
        return false;
    }
    // Клик уже сделан (подтверждение приходит после него): строка поколения обязана быть в журнале.
    match run.wait_page_log_line(&format!("{GENERATION_LINE_PREFIX}dub"), ACK_TIMEOUT) {
        Ok(_) => true,
        Err(error) => {
            failed_steps.push(format!("page-switched-dub: {error}"));
            false
        }
    }
}

/// Ждёт, пока страница САМА перейдёт на выбранную озвучку (её нажал экстеншен по памяти).
/// Возвращает, через сколько после `since` это случилось.
fn wait_until_extension_applies_memory(
    run: &HandoffRun<'_>,
    since: Instant,
    failed_steps: &mut Vec<String>,
) -> Option<Duration> {
    let needle = format!("{GENERATION_LINE_PREFIX}dub episode=1 dub={PICKED_DUB}");
    match run.wait_page_log_line(&needle, APPLY_TIMEOUT) {
        Ok(_) => Some(since.elapsed()),
        Err(error) => {
            failed_steps.push(format!("extension-applies-memory: {error}"));
            None
        }
    }
}

/// Вкладка стенда глазами экстеншена: приватная ли она. `None` — не удалось выяснить.
fn stand_tab_is_private(run: &mut HandoffRun<'_>, failed_steps: &mut Vec<String>) -> Option<bool> {
    let prefix = run.page_url_prefix();
    let raw = match run.browser.run_in_service_page(&tabs_script()) {
        Ok(raw) => raw,
        Err(error) => {
            failed_steps.push(format!("tabs: {error}"));
            return None;
        }
    };
    match parse_tabs(&raw) {
        Ok(tabs) => tabs
            .iter()
            .find(|tab| tab.url.starts_with(&prefix))
            .map(|tab| tab.incognito),
        Err(error) => {
            failed_steps.push(format!("tabs: {error}"));
            None
        }
    }
}

/// Сеанс 1 «restart»: выбор зрителя на странице и запись в память. Возвращает память после выбора.
fn pick_and_remember(
    run: &mut HandoffRun<'_>,
    failed_steps: &mut Vec<String>,
) -> Option<StoredMemory> {
    if !wait_for_grant(run, failed_steps) || !wait_for_first_frames(run, failed_steps) {
        return None;
    }
    if !open_service_page(run, failed_steps) || !viewer_picks_dub_on_page(run, failed_steps) {
        return None;
    }
    wait_for_frame_growth(run, "frames-after-page-pick", failed_steps);
    read_memory_when_written(run, failed_steps)
}

/// Сеанс 2 «restart»: память применяется сама; затем «забыть». Возвращает задержку применения и
/// память после «забыть».
fn restore_then_forget(
    run: &mut HandoffRun<'_>,
    session_started_at: Instant,
    failed_steps: &mut Vec<String>,
) -> (Option<Duration>, Option<StoredMemory>) {
    if !wait_for_grant(run, failed_steps) || !wait_for_first_frames(run, failed_steps) {
        return (None, None);
    }
    let apply_delay = wait_until_extension_applies_memory(run, session_started_at, failed_steps);
    wait_for_frame_growth(run, "frames-after-restore", failed_steps);
    if !open_service_page(run, failed_steps) {
        return (apply_delay, None);
    }
    let reply = run
        .browser
        .run_in_service_page(&forget_script())
        .map_err(|error| error.to_string())
        .and_then(|raw| parse_forget_reply(&raw));
    match reply {
        Ok(ForgetReply::Forgotten) => {}
        Ok(ForgetReply::Failed { reason }) => {
            failed_steps.push(format!("forget: failed: {reason}"))
        }
        Err(error) => failed_steps.push(format!("forget: {error}")),
    }
    (apply_delay, read_memory_once(run, failed_steps))
}

/// Сеанс 3 «restart»: окно тишины. Возвращает память в конце окна.
fn watch_quiet_window(
    run: &mut HandoffRun<'_>,
    session_started_at: Instant,
    failed_steps: &mut Vec<String>,
) -> Option<StoredMemory> {
    if !wait_for_grant(run, failed_steps) || !wait_for_first_frames(run, failed_steps) {
        return None;
    }
    if !open_service_page(run, failed_steps) {
        return None;
    }
    wait_for_frame_growth(run, "frames-without-memory", failed_steps);
    // Окно считается от передачи, как и задержка применения в сеансе 2.
    if let Some(left) = QUIET_WINDOW.checked_sub(session_started_at.elapsed()) {
        std::thread::sleep(left);
    }
    read_memory_once(run, failed_steps)
}

fn drive_restart(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
) -> Result<Verdict, ScenarioError> {
    let fingerprint = bench_fingerprint(stand);
    // Сеанс 1: зритель выбрал Dub B на странице.
    let mut picked_steps = Vec::new();
    let plan = SessionPlan {
        window_privacy: WindowPrivacy::Normal,
        page_pick: Some(PAGE_PICK),
    };
    let mut run = match begin_session(stand, environment, browser, plan)? {
        Ok(run) => run,
        Err(verdict) => return Ok(verdict),
    };
    let memory_after_pick = pick_and_remember(&mut run, &mut picked_steps);
    let picked = end_session(run, environment, &mut picked_steps)?;
    // Сеанс 2: тот же профиль, страница сама ничего не жмёт.
    let mut restored_steps = Vec::new();
    let session_started_at = Instant::now();
    let plan = SessionPlan {
        window_privacy: WindowPrivacy::Normal,
        page_pick: None,
    };
    let mut run = match begin_session(stand, environment, browser, plan)? {
        Ok(run) => run,
        Err(verdict) => return Ok(verdict),
    };
    let (apply_delay, memory_after_forget) =
        restore_then_forget(&mut run, session_started_at, &mut restored_steps);
    let restored = end_session(run, environment, &mut restored_steps)?;
    // Сеанс 3: после «забыть».
    let mut forgotten_steps = Vec::new();
    let session_started_at = Instant::now();
    let mut run = match begin_session(stand, environment, browser, plan)? {
        Ok(run) => run,
        Err(verdict) => return Ok(verdict),
    };
    let memory_in_forgotten_session =
        watch_quiet_window(&mut run, session_started_at, &mut forgotten_steps);
    let forgotten = end_session(run, environment, &mut forgotten_steps)?;
    Ok(judge_restart(&RestartEvidence {
        page: SERIES_PAGE,
        fingerprint,
        picked,
        picked_failed_steps: picked_steps,
        memory_after_pick,
        restored,
        restored_failed_steps: restored_steps,
        restored_apply_delay: apply_delay,
        memory_after_forget,
        forgotten,
        forgotten_failed_steps: forgotten_steps,
        memory_in_forgotten_session,
        quiet_window: QUIET_WINDOW,
    }))
}

/// Подсаженная запись: Dub B, `used_at` — час назад.
fn seeded_choice() -> Result<StoredSeriesChoice, String> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock is before 1970: {error}"))?
        .as_millis();
    let used_at = u64::try_from(now_ms.saturating_sub(SEEDED_AGE.as_millis()))
        .map_err(|error| format!("clock value does not fit u64: {error}"))?;
    Ok(StoredSeriesChoice {
        audio: Some(PICKED_DUB_LABEL.to_owned()),
        quality: None,
        subtitles: None,
        used_at,
    })
}

/// Приватный сеанс (а): выбор на странице не пишется. Затем стенд подсаживает запись (б).
/// Возвращает: видна ли вкладка как приватная, память после паузы.
fn private_pick_leaves_no_trace(
    run: &mut HandoffRun<'_>,
    fingerprint: &str,
    seeded: &StoredSeriesChoice,
    failed_steps: &mut Vec<String>,
) -> (Option<bool>, Option<StoredMemory>) {
    if !wait_for_grant(run, failed_steps) || !wait_for_first_frames(run, failed_steps) {
        return (None, None);
    }
    if !open_service_page(run, failed_steps) {
        return (None, None);
    }
    let private = stand_tab_is_private(run, failed_steps);
    if !viewer_picks_dub_on_page(run, failed_steps) {
        return (private, None);
    }
    wait_for_frame_growth(run, "frames-after-private-pick", failed_steps);
    // Пауза «чтобы запись, будь она, точно появилась», затем память должна быть пустой.
    std::thread::sleep(PRIVATE_SETTLE);
    let memory = read_memory_once(run, failed_steps);
    // Подсадка для сеанса (б): прямо в хранилище, как будто зритель выбирал в обычном окне раньше.
    let audio = seeded.audio.as_deref().unwrap_or(PICKED_DUB_LABEL);
    match run
        .browser
        .run_in_service_page(&seed_script(fingerprint, audio, seeded.used_at))
    {
        Ok(_) => {}
        Err(error) => failed_steps.push(format!("seed-memory: {error}")),
    }
    (private, memory)
}

/// Приватный сеанс (б): подсаженная память применяется и остаётся нетронутой.
fn private_read_applies_without_writing(
    run: &mut HandoffRun<'_>,
    session_started_at: Instant,
    failed_steps: &mut Vec<String>,
) -> (Option<bool>, Option<StoredMemory>) {
    if !wait_for_grant(run, failed_steps) || !wait_for_first_frames(run, failed_steps) {
        return (None, None);
    }
    if !open_service_page(run, failed_steps) {
        return (None, None);
    }
    let private = stand_tab_is_private(run, failed_steps);
    wait_until_extension_applies_memory(run, session_started_at, failed_steps);
    wait_for_frame_growth(run, "frames-after-private-restore", failed_steps);
    std::thread::sleep(PRIVATE_SETTLE);
    (private, read_memory_once(run, failed_steps))
}

fn drive_incognito(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
) -> Result<Verdict, ScenarioError> {
    let fingerprint = bench_fingerprint(stand);
    let seeded = match seeded_choice() {
        Ok(seeded) => seeded,
        Err(reason) => return Ok(Verdict::fail(reason)),
    };
    // (а) выбор в приватном окне.
    let mut picked_steps = Vec::new();
    let plan = SessionPlan {
        window_privacy: WindowPrivacy::Private,
        page_pick: Some(PAGE_PICK),
    };
    let mut run = match begin_session(stand, environment, browser, plan)? {
        Ok(run) => run,
        Err(verdict) => return Ok(verdict),
    };
    let (first_private, memory_after_pick) =
        private_pick_leaves_no_trace(&mut run, &fingerprint, &seeded, &mut picked_steps);
    let picked = end_session(run, environment, &mut picked_steps)?;
    // (б) чтение в приватном окне.
    let mut restored_steps = Vec::new();
    let session_started_at = Instant::now();
    let plan = SessionPlan {
        window_privacy: WindowPrivacy::Private,
        page_pick: None,
    };
    let mut run = match begin_session(stand, environment, browser, plan)? {
        Ok(run) => run,
        Err(verdict) => return Ok(verdict),
    };
    let (second_private, memory_after_restore) =
        private_read_applies_without_writing(&mut run, session_started_at, &mut restored_steps);
    let restored = end_session(run, environment, &mut restored_steps)?;
    Ok(judge_incognito(&IncognitoEvidence {
        page: SERIES_PAGE,
        fingerprint,
        stand_tab_is_private: [first_private, second_private],
        picked,
        picked_failed_steps: picked_steps,
        memory_after_pick,
        seeded,
        restored,
        restored_failed_steps: restored_steps,
        memory_after_restore,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::page_catalog::PAGE_SCENARIOS;

    fn page_source() -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("site/pages/{SERIES_PAGE}.html")),
        )
        .expect("page file")
    }

    #[test]
    fn the_page_understands_the_pick_the_scenarios_send() {
        assert!(
            PAGE_SCENARIOS
                .iter()
                .any(|scenario| scenario.id == SERIES_PAGE)
        );
        let source = page_source();
        // Параметр адреса и хук, который шлёт робот; подпись кнопки — та, что ждёт приговор.
        assert!(source.contains("ctx.params.raw.get('pick_dub')"));
        assert!(source.contains(&format!("'{}'", PICK_COMMAND.trim_start_matches("hook:"))));
        assert!(source.contains(&format!("id: '{PICKED_DUB}', label: '{PICKED_DUB_LABEL}'")));
        // Без параметра хуков нет: поведение страницы прежнее.
        assert!(source.contains("pickDub === null ? {}"));
    }

    #[test]
    fn the_pick_is_written_into_the_page_address_in_the_documented_syntax() {
        assert_eq!(PAGE_PICK.dub_id, "dub-b");
        let plan = SessionPlan {
            window_privacy: WindowPrivacy::Normal,
            page_pick: Some(PAGE_PICK),
        };
        assert_eq!(plan.page_query(), "&mode=handoff&pick_dub=dub-b@1500");
        let plain = SessionPlan {
            window_privacy: WindowPrivacy::Normal,
            page_pick: None,
        };
        assert_eq!(plain.page_query(), "&mode=handoff");
    }

    #[test]
    fn the_control_window_is_longer_than_the_pick_delay_and_the_seed_is_in_the_past() {
        assert!(QUIET_WINDOW > Duration::from_millis(u64::from(PAGE_PICK.delay_ms)));
        let seeded = seeded_choice().expect("clock");
        assert_eq!(seeded.audio.as_deref(), Some(PICKED_DUB_LABEL));
        let now_ms = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_millis(),
        )
        .expect("fits");
        assert!(seeded.used_at < now_ms - 3_000_000, "{}", seeded.used_at);
    }
}
