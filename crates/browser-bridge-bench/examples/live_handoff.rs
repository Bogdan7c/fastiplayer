//! Живой стенд одной командой: настоящий экстеншен, посредник и плеер на живом сайте.
//!
//! Браузер запускается в частном окружении со свежим профилем: автоматика стенда не трогает
//! профиль и шину владельца. Страница открывается, робот раз в секунду зовёт передачу вкладки, пока
//! она не состоится (там, где сайт ждёт клика по play, это успевает сделать человек). Дальше помощник
//! печатает строки журнала плеера про рекламу и концы сеанса. С `--press auto` он сам жмёт `Next`
//! (MPRIS), как только плашка предложит кнопку.
//!
//! ```text
//! cargo run --release -p browser-bridge-bench --example live_handoff -- \
//!   --browser firefox --display virtual --url <адрес> --seconds 90 --press auto
//! ```
//!
//! С `--hand-off person` робот ничего не передаёт: браузер просто открыт, значок нажимает человек
//! (или настоящая мышь снаружи), а помощник печатает журнал плеера. С `--stand-page <страница>`
//! вместо живого сайта открывается страница стенда, сервер стенда живёт весь прогон:
//!
//! ```text
//! cargo run --release -p browser-bridge-bench --example live_handoff -- \
//!   --display real --hand-off person --stand-page banner-with-main --seconds 300
//! ```
//!
//! `--stand-page` можно повторить: каждая страница откроется своей вкладкой, активной останется
//! последняя. `--autoplay browser-default` запускает браузер с его обычными настройками автозапуска
//! (по умолчанию автозапуск разрешён, как на стенде).
//!
//! Поздний вход (10d): `--extension-arrives late-install` запускает браузер без экстеншена и ставит
//! его после паузы `--wait`, при уже открытой вкладке. `--update-from <каталог>` ставит экстеншен до
//! страницы и после той же паузы обновляет его сборкой из каталога (там версия должна быть новее).
//! Значок и кнопку нажимает настоящая мышь снаружи (`--hand-off person`).
//!
//! Возврат вкладки (11b2): `--tab-return-after <с>` — через столько секунд после передачи помощник ставит
//! окно плеера на паузу (MPRIS), закрывает вкладку страницы, ждёт и жмёт play в окне: экстеншен должен
//! открыть страницу заново, и тот же сеанс продолжится в том же окне. Журнал сироты и возврата печатается.
//!
//! Режим 2 «автозапуск» (12b): `--auto-play on` включает его для всех сайтов («Play in Fastiplayer»),
//! `--auto-play ask-first` — включает со словом «Ask first» для нового сайта (на видео — предложение «Ask»).
//! Настройка пишется в `storage.local` экстеншена сразу после запуска браузера, до первой страницы.
//!
//! Качество (13c): `--seek-after <с> --seek-by <с>` — через столько секунд после передачи окно плеера
//! перематывается (MPRIS) на столько секунд; перемотка за буфер заставляет ABR страницы качать
//! качество под нынешний размер её плеера, и кнопка качества показывает смену.
//!
//! `--youtube-quality <с>:<уровень>` (можно повторять) — через столько секунд после передачи плеер
//! YouTube на вкладке просят держать уровень (`small` = 240p, `hd720`…) его же API, как ручной выбор
//! в меню сайта: служебная страница экстеншена вызывает его в главном мире страницы.
//!
//! Сайты, где видео стартует только по клику (Ashdi, Tortuga, 13b2): `--click-at <x>,<y>` — клик настоящей
//! мышью стенда по точке невидимого экрана после паузы `--wait`, до передачи. `--screenshots <каталог>` —
//! снимки невидимого экрана (до клика, после клика и каждые 15 с после передачи) копируются в каталог:
//! по первому снимку находят точку кнопки play, по остальным видно окно плеера. Только `--display virtual`.
//!
//! Экстеншен должен быть собран (`npm run build` в `extensions/browser`), плеер берётся из
//! `target/release`. Адреса и журнал остаются только в выводе: в отчёты их не копировать.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use browser_bridge_bench::browsers::{
    BrowserKind, BrowserUnderTest, ExtensionSource, HandOffOutcome, LaunchOptions, TabHandle,
    launch,
};
use browser_bridge_bench::desktop_input::{DesktopInput, Point};
use browser_bridge_bench::environment::{
    BenchEnvironment, DesktopSetup, DisplayTarget, EnvironmentOptions, PlayerConfigProfile,
};
use browser_bridge_bench::media::ensure_media;
use browser_bridge_bench::player_control::PlayerControl;
use browser_bridge_bench::screen_capture::ScreenCapture;
use browser_bridge_bench::site::{StandSite, StandSiteConfig};

/// Строки журнала плеера, которые интересны человеку.
const INTERESTING_LINES: &[&str] = &[
    "Ad plate",
    "Ad skip",
    "refused: an ad",
    "Browser session end notice",
    "stream_rejected",
    "ERROR",
    "ClosedBy",
    "Browser session ended",
    // Возврат вкладки (11b2): уход страницы, просьба, итог, слова окна.
    "followed the page link",
    "bring the page back",
    "The page came back",
    "could not be brought back",
    "about the page reopen",
    // Субтитры страницы (13b2): документ дошёл до склада плеера, дорожка разобрана или отвергнута.
    "Page subtitle",
    // Качество (13c): подпись кнопки качества сменилась, зритель попросил вкладку страницы.
    "Browser quality shown",
    "Show page requested",
];
/// Сколько окно стоит на паузе до закрытия вкладки и сколько сирота ждёт до play (`--tab-return-after`).
const TAB_RETURN_PAUSE: Duration = Duration::from_secs(1);
const TAB_RETURN_ORPHAN_WAIT: Duration = Duration::from_secs(3);
/// Подстрока журнала: плашка предлагает нашу кнопку «Skip» (подсказку `on_page` робот не жмёт).
const SKIP_OFFER: &str = "skip=skip";
/// Как часто просим передачу, пока видео нет.
const HAND_OFF_RETRY: Duration = Duration::from_secs(1);
/// Сколько даём странице прогрева.
const WARM_UP: Duration = Duration::from_secs(5);
/// Сколько ждём после клика по play, прежде чем снять экран и звать передачу: сайт заводит видео.
const AFTER_CLICK: Duration = Duration::from_secs(3);
/// Как часто снимаем экран после передачи (`--screenshots`).
const SCREENSHOT_EVERY: Duration = Duration::from_secs(15);
/// Шаг чтения журнала.
const LOG_POLL: Duration = Duration::from_millis(200);

/// Параметр адреса: страница стенда ждёт команд и не завершается сама.
const STAND_HANDOFF_QUERY: &str = "&mode=handoff";

/// Что открыть в браузере.
enum Target {
    /// Живой сайт.
    Url(String),
    /// Страницы стенда по их именам в `site/pages`, каждая в своей вкладке.
    StandPages(Vec<String>),
}

/// Кто отдаёт видео в плеер.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HandOff {
    /// Робот раз в секунду зовёт команду передачи.
    Robot,
    /// Человек жмёт значок экстеншена, робот только показывает журнал.
    Person,
}

/// Кто жмёт кнопку плашки.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Press {
    /// Никто: смотрит или жмёт человек.
    Nobody,
    /// Робот, сразу как только кнопка появилась.
    Robot,
}

/// Когда экстеншен появляется в браузере.
enum ExtensionArrival {
    /// При запуске браузера, как в обычном прогоне.
    AtStart,
    /// После паузы `--wait`, при уже открытой вкладке.
    LateInstall,
    /// Стоит до страницы, после паузы обновляется сборкой из этого каталога.
    LateUpdate(PathBuf),
}

struct Arguments {
    browser: BrowserKind,
    display: DisplayTarget,
    target: Target,
    hand_off: HandOff,
    /// Что открыть перед видео, чтобы сайт завёл свежему профилю куки.
    warm_up_url: Option<String>,
    seconds: u64,
    /// Сколько ждать между открытием страницы и первой передачей.
    wait_seconds: u64,
    press: Press,
    /// Разрешить автозапуск без жеста, как на стенде (`false` — обычные настройки браузера).
    autoplay_without_gesture: bool,
    extension_arrival: ExtensionArrival,
    /// Через сколько секунд после передачи закрыть вкладку и попросить её назад (`None` — не просить).
    tab_return_after: Option<Duration>,
    /// Режим 2: `None` — настройка экстеншена по умолчанию (выключен).
    auto_play: Option<AutoPlay>,
    /// Клик мышью стенда по этой точке невидимого экрана перед передачей (`None` — не кликать).
    click_at: Option<Point>,
    /// Куда копировать снимки невидимого экрана (`None` — не снимать).
    screenshots: Option<PathBuf>,
    /// Перемотка окна плеера по MPRIS после передачи (`None` — не перематывать).
    seek: Option<SeekPlan>,
    /// Смены качества плеером YouTube после передачи (`--youtube-quality`), по порядку.
    youtube_qualities: Vec<YoutubeQualityStep>,
}

/// Смена качества на странице YouTube (13c): API плеера сайта, как ручной выбор в его меню.
struct YoutubeQualityStep {
    /// Через сколько после передачи.
    after: Duration,
    /// Уровень YouTube: `tiny` (144p), `small` (240p), `medium` (360p), `large` (480p), `hd720`…
    level: String,
}

/// Функция служебной страницы: находит вкладку YouTube и в ГЛАВНОМ мире страницы просит её плеер
/// держать уровень `__LEVEL__` (`setPlaybackQualityRange`). Возвращает строку для журнала помощника.
const YOUTUBE_QUALITY_SCRIPT: &str = r#"async (api) => {
  const tabs = await api.tabs.query({ url: "*://*.youtube.com/*" });
  if (tabs.length === 0) return "no YouTube tab";
  const [injection] = await api.scripting.executeScript({
    target: { tabId: tabs[0].id },
    world: "MAIN",
    args: ["__LEVEL__"],
    func: (level) => {
      const player = document.getElementById("movie_player");
      if (!player || typeof player.setPlaybackQualityRange !== "function") return "no player API";
      player.setPlaybackQualityRange(level, level);
      return "asked " + level + ", the page says " + player.getPlaybackQuality();
    },
  });
  return String(injection && injection.result);
}"#;

/// Перемотка из окна плеера (13c): за буфер страницы, чтобы её ABR сразу качал новое качество.
#[derive(Clone, Copy)]
struct SeekPlan {
    /// Через сколько после передачи (`--seek-after`).
    after: Duration,
    /// На сколько секунд вперёд, со знаком (`--seek-by`).
    by_seconds: i64,
}

/// Точка экрана из `x,y`.
fn parse_point(value: &str) -> Result<Point, String> {
    let (x, y) = value
        .split_once(',')
        .ok_or_else(|| format!("--click-at expects x,y, got {value}"))?;
    let coordinate = |text: &str| {
        text.trim()
            .parse::<u32>()
            .map_err(|error| format!("--click-at {value}: {error}"))
    };
    Ok(Point::new(coordinate(x)?, coordinate(y)?))
}

/// Как включить режим 2 «автозапуск» (12b) в свежем профиле.
#[derive(Clone, Copy, Debug)]
enum AutoPlay {
    /// Включён, новый сайт — сразу в плеер (`unknown_site_rule: always`).
    On,
    /// Включён, новый сайт — предложение «Ask» на видео (`unknown_site_rule: ask`).
    AskFirst,
}

impl AutoPlay {
    /// Скрипт страницы экстеншена: записать настройку режима 2 целиком (форма `autoHandoffSettings`).
    fn write_script(self) -> String {
        let unknown_site_rule = match self {
            Self::On => "always",
            Self::AskFirst => "ask",
        };
        format!(
            "async (api) => {{ await api.storage.local.set({{ autoHandoffSettings: {{ enabled: true, \
             min_duration_s: 60, unknown_site_rule: {unknown_site_rule:?}, site_rules: {{}}, own_stop_list: [] }} }}); \
             return \"written\"; }}"
        )
    }
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut browser = BrowserKind::Firefox;
    let mut display = DisplayTarget::Virtual;
    let mut target = None;
    let mut stand_pages = Vec::new();
    let mut autoplay_without_gesture = true;
    let mut hand_off = HandOff::Robot;
    let mut warm_up_url = None;
    let mut seconds = 60;
    let mut wait_seconds = 0;
    let mut press = Press::Nobody;
    let mut extension_arrival = ExtensionArrival::AtStart;
    let mut tab_return_after = None;
    let mut auto_play = None;
    let mut click_at = None;
    let mut screenshots = None;
    let mut seek_after = None;
    let mut seek_by = None;
    let mut youtube_qualities = Vec::new();
    let mut arguments = std::env::args().skip(1);
    while let Some(name) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("{name} needs a value"))?;
        match (name.as_str(), value.as_str()) {
            ("--browser", "firefox") => browser = BrowserKind::Firefox,
            ("--browser", "chromium") => browser = BrowserKind::Chromium,
            ("--display", "virtual") => display = DisplayTarget::Virtual,
            ("--display", "real") => display = DisplayTarget::Real,
            ("--press", "auto") => press = Press::Robot,
            ("--press", "none") => press = Press::Nobody,
            ("--hand-off", "robot") => hand_off = HandOff::Robot,
            ("--hand-off", "person") => hand_off = HandOff::Person,
            ("--url", _) => target = Some(Target::Url(value)),
            ("--stand-page", _) => stand_pages.push(value),
            ("--autoplay", "allowed") => autoplay_without_gesture = true,
            ("--autoplay", "browser-default") => autoplay_without_gesture = false,
            ("--extension-arrives", "at-start") => extension_arrival = ExtensionArrival::AtStart,
            ("--extension-arrives", "late-install") => {
                extension_arrival = ExtensionArrival::LateInstall;
            }
            ("--update-from", _) => {
                extension_arrival = ExtensionArrival::LateUpdate(PathBuf::from(value));
            }
            ("--warm-up-url", _) => warm_up_url = Some(value),
            ("--auto-play", "on") => auto_play = Some(AutoPlay::On),
            ("--auto-play", "ask-first") => auto_play = Some(AutoPlay::AskFirst),
            ("--seconds", _) => {
                seconds = value
                    .parse()
                    .map_err(|error| format!("--seconds: {error}"))?;
            }
            ("--tab-return-after", _) => {
                let after: u64 = value
                    .parse()
                    .map_err(|error| format!("--tab-return-after: {error}"))?;
                tab_return_after = Some(Duration::from_secs(after));
            }
            ("--wait", _) => {
                wait_seconds = value.parse().map_err(|error| format!("--wait: {error}"))?;
            }
            ("--click-at", _) => click_at = Some(parse_point(&value)?),
            ("--screenshots", _) => screenshots = Some(PathBuf::from(value)),
            ("--seek-after", _) => {
                let after: u64 = value
                    .parse()
                    .map_err(|error| format!("--seek-after: {error}"))?;
                seek_after = Some(Duration::from_secs(after));
            }
            ("--youtube-quality", _) => youtube_qualities.push(parse_youtube_quality(&value)?),
            ("--seek-by", _) => {
                seek_by = Some(
                    value
                        .parse::<i64>()
                        .map_err(|error| format!("--seek-by: {error}"))?,
                );
            }
            _ => return Err(format!("unknown argument {name} {value}")),
        }
    }
    if !stand_pages.is_empty() {
        target = Some(Target::StandPages(stand_pages));
    }
    // Перемотке нужны оба числа: одно без другого — ошибка вызова, а не тихий пропуск.
    let seek = match (seek_after, seek_by) {
        (Some(after), Some(by_seconds)) => Some(SeekPlan { after, by_seconds }),
        (None, None) => None,
        _ => return Err("--seek-after and --seek-by go together".to_owned()),
    };
    Ok(Arguments {
        browser,
        display,
        target: target.ok_or("--url or --stand-page is required")?,
        hand_off,
        warm_up_url,
        seconds,
        wait_seconds,
        press,
        autoplay_without_gesture,
        extension_arrival,
        tab_return_after,
        auto_play,
        click_at,
        screenshots,
        seek,
        youtube_qualities,
    })
}

/// Шаг `--youtube-quality <с>:<уровень>`. Уровень — только буквы и цифры: он попадает в текст скрипта.
fn parse_youtube_quality(value: &str) -> Result<YoutubeQualityStep, String> {
    let (after, level) = value
        .split_once(':')
        .ok_or_else(|| format!("--youtube-quality expects <seconds>:<level>, got {value}"))?;
    let after: u64 = after
        .parse()
        .map_err(|error| format!("--youtube-quality {value}: {error}"))?;
    if level.is_empty() || !level.chars().all(|symbol| symbol.is_ascii_alphanumeric()) {
        return Err(format!(
            "--youtube-quality {value}: the level must be like small or hd720"
        ));
    }
    Ok(YoutubeQualityStep {
        after: Duration::from_secs(after),
        level: level.to_owned(),
    })
}

/// Просит плеер YouTube на вкладке сменить качество (служебная страница экстеншена открывается
/// фоновой один раз). Итог и сбои печатаются: это помощник человека.
fn ask_youtube_quality(
    browser: &mut dyn BrowserUnderTest,
    tab: &TabHandle,
    service_page_open: &mut bool,
    level: &str,
) {
    if !*service_page_open {
        if let Err(error) = browser.open_service_page(tab) {
            println!("youtube quality {level}: the service page did not open: {error}");
            return;
        }
        *service_page_open = true;
    }
    let script = YOUTUBE_QUALITY_SCRIPT.replace("__LEVEL__", level);
    match browser.run_in_service_page(&script) {
        Ok(reply) => println!("youtube quality {level}: {reply}"),
        Err(error) => println!("youtube quality {level}: failed: {error}"),
    }
}

/// Снимки невидимого экрана для человека: файл снимается в каталог прогона и копируется в `--screenshots`.
struct Screenshots<'env> {
    capture: ScreenCapture<'env>,
    destination: PathBuf,
}

impl Screenshots<'_> {
    /// Снять экран с меткой; сбой печатается и прогон не рвёт: это помощник человека.
    fn take(&self, label: &str) {
        let shot = match self.capture.take_screenshot(label) {
            Ok(shot) => shot,
            Err(error) => {
                println!("screenshot {label} failed: {error}");
                return;
            }
        };
        let target = self.destination.join(shot.file_name().unwrap_or_default());
        match fs::copy(&shot, &target) {
            Ok(_) => println!("screenshot: {}", target.display()),
            Err(error) => println!("screenshot {label}: copy failed: {error}"),
        }
    }
}

/// Новые строки журнала после `offset`; неполная последняя строка ждёт следующего чтения.
fn read_new_lines(path: &Path, offset: &mut usize) -> Vec<String> {
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    let Some(fresh) = bytes.get(*offset..) else {
        return Vec::new();
    };
    let Some(end) = fresh.iter().rposition(|byte| *byte == b'\n') else {
        return Vec::new();
    };
    let lines = String::from_utf8_lossy(&fresh[..end])
        .lines()
        .map(str::to_owned)
        .collect();
    *offset += end + 1;
    lines
}

/// Поднимает сервер стенда: он должен жить, пока открыта страница.
fn start_stand_site(repository: &Path) -> Result<StandSite, String> {
    let media = ensure_media(&repository.join("target/browser-bridge-bench/media"))
        .map_err(|error| error.to_string())?;
    StandSite::start(StandSiteConfig {
        site_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("site"),
        media,
    })
    .map_err(|error| error.to_string())
}

/// Адрес страницы стенда в режиме передачи.
fn stand_page_url(site: &StandSite, page: &str) -> String {
    let main_origin = site.main_origin();
    let other_origin = site.other_origin();
    format!(
        "{main_origin}/site/pages/{page}.html?run=live-{page}&report_origin={main_origin}&other_origin={other_origin}{STAND_HANDOFF_QUERY}"
    )
}

/// Зовёт передачу, пока она не состоится или не выйдет время. Возвращает, состоялась ли она.
fn hand_off_by_robot(
    browser: &mut dyn BrowserUnderTest,
    tab: &TabHandle,
    url: &str,
    deadline: Instant,
) -> bool {
    while Instant::now() < deadline {
        match browser.hand_off_tab(tab, url) {
            Ok(HandOffOutcome::Started) => return true,
            Ok(other) => println!("hand-off: {other:?}"),
            Err(error) => println!("hand-off failed: {error}"),
        }
        sleep(HAND_OFF_RETRY);
    }
    false
}

fn run(arguments: &Arguments) -> Result<(), String> {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    // Сервер стенда держим до конца функции, иначе страница потеряет файлы.
    let stand_site = match &arguments.target {
        Target::StandPages(_) => Some(start_stand_site(&repository)?),
        Target::Url(_) => None,
    };
    let urls: Vec<String> = match (&arguments.target, &stand_site) {
        (Target::Url(url), _) => vec![url.clone()],
        (Target::StandPages(pages), Some(site)) => pages
            .iter()
            .map(|page| stand_page_url(site, page))
            .collect(),
        (Target::StandPages(_), None) => return Err("the stand site did not start".to_owned()),
    };
    let environment = BenchEnvironment::create(EnvironmentOptions {
        display: arguments.display,
        player_bin_dir: repository.join("target/release"),
        // На настоящем экране — настройки владельца, в невидимом kwin — чистые.
        player_config: if arguments.display == DisplayTarget::Real {
            PlayerConfigProfile::Gles2Vaapi
        } else {
            PlayerConfigProfile::Clean
        },
        player_rust_log: "info".to_owned(),
        work_root: std::env::temp_dir(),
        // Мышь и снимки экрана стенда — только когда их попросили: обычный прогон их не трогает.
        desktop: if arguments.click_at.is_some() || arguments.screenshots.is_some() {
            DesktopSetup::pointer_stand()
        } else {
            DesktopSetup::default()
        },
    })
    .map_err(|error| error.to_string())?;
    if (arguments.click_at.is_some() || arguments.screenshots.is_some())
        && arguments.display != DisplayTarget::Virtual
    {
        return Err("--click-at and --screenshots work only with --display virtual".to_owned());
    }
    let screenshots = match &arguments.screenshots {
        Some(destination) => {
            fs::create_dir_all(destination).map_err(|error| format!("--screenshots: {error}"))?;
            let capture = ScreenCapture::new(
                &environment,
                &ScreenCapture::icons_dir(&repository, arguments.browser),
            )
            .map_err(|error| error.to_string())?;
            Some(Screenshots {
                capture,
                destination: destination.clone(),
            })
        }
        None => None,
    };
    environment
        .connect_browser(arguments.browser)
        .map_err(|error| error.to_string())?;
    let flavour = match arguments.browser {
        BrowserKind::Chromium => "chrome",
        BrowserKind::Firefox => "firefox",
    };
    let built_extension = ExtensionSource::UnpackedDir(
        // `FASTIPLAYER_LIVE_EXTENSION_DIST`: другая сборка экстеншена, для сравнения.
        std::env::var_os("FASTIPLAYER_LIVE_EXTENSION_DIST")
            .map_or_else(|| repository.join("extensions/browser/dist"), PathBuf::from)
            .join(flavour),
    );
    let arrives_at_start = matches!(arguments.extension_arrival, ExtensionArrival::AtStart);
    let mut browser = launch(
        arguments.browser,
        &environment,
        LaunchOptions {
            extensions: if arrives_at_start {
                vec![built_extension.clone()]
            } else {
                Vec::new()
            },
            autoplay_without_gesture: arguments.autoplay_without_gesture,
            loopback_host_aliases: Vec::new(),
            runtime_extension_install: !arrives_at_start,
        },
    )
    .map_err(|error| error.to_string())?;
    if matches!(arguments.extension_arrival, ExtensionArrival::LateUpdate(_)) {
        browser
            .install_extension_at_runtime(&built_extension)
            .map_err(|error| error.to_string())?;
    }
    // Режим 2 (12b): настройка ложится до первой страницы; контент-скрипты прочтут её при загрузке.
    if let Some(auto_play) = arguments.auto_play {
        if !arrives_at_start {
            return Err("--auto-play needs the extension at start".to_owned());
        }
        let reply = browser
            .run_in_extension_page(&auto_play.write_script())
            .map_err(|error| format!("--auto-play: {error}"))?;
        println!("auto play {auto_play:?}: {reply}");
    }
    // Свежий профиль на первом же адресе рекламы почти не видит: сайту нужен предыдущий заход.
    if let Some(warm_up_url) = &arguments.warm_up_url {
        browser
            .open_tab(warm_up_url)
            .map_err(|error| error.to_string())?;
        sleep(WARM_UP);
    }
    // Передачу робот зовёт на последней вкладке: она и остаётся активной.
    let mut opened = None;
    for url in &urls {
        let tab = browser.open_tab(url).map_err(|error| error.to_string())?;
        opened = Some((tab, url));
    }
    let (tab, url) = opened.ok_or("nothing to open")?;
    // Пауза до передачи: реклама в начале ролика должна успеть начаться.
    sleep(Duration::from_secs(arguments.wait_seconds));
    if let Some(shots) = &screenshots {
        shots.take("before-click");
    }
    if let Some(point) = arguments.click_at {
        let endpoint = environment
            .desktop_endpoint()
            .map_err(|error| error.to_string())?;
        let input = DesktopInput::connect(&endpoint).map_err(|error| error.to_string())?;
        input
            .mouse()
            .click_at(point)
            .map_err(|error| error.to_string())?;
        println!("clicked at {},{}", point.x, point.y);
        sleep(AFTER_CLICK);
        if let Some(shots) = &screenshots {
            shots.take("after-click");
        }
    }
    let late_extension = match &arguments.extension_arrival {
        ExtensionArrival::AtStart => None,
        ExtensionArrival::LateInstall => Some(built_extension),
        ExtensionArrival::LateUpdate(directory) => {
            Some(ExtensionSource::UnpackedDir(directory.clone()))
        }
    };
    if let Some(extension) = &late_extension {
        browser
            .install_extension_at_runtime(extension)
            .map_err(|error| error.to_string())?;
        println!("extension arrived while the tab is open");
    }

    let deadline = Instant::now() + Duration::from_secs(arguments.seconds);
    let started = match arguments.hand_off {
        HandOff::Robot => hand_off_by_robot(browser.as_mut(), &tab, url, deadline),
        HandOff::Person => true,
    };
    println!("hand-off started: {started}");

    let control = PlayerControl::on_bus(
        environment
            .bus_address()
            .map_err(|error| error.to_string())?,
    );
    let verbose = std::env::var_os("FASTIPLAYER_LIVE_ALL_LINES").is_some();
    let mut offset = 0;
    let mut offer_waiting = false;
    let tab_return_at = arguments
        .tab_return_after
        .map(|after| Instant::now() + after);
    let mut tab_closed = false;
    let mut seek_at = arguments
        .seek
        .map(|plan| (Instant::now() + plan.after, plan.by_seconds));
    let handed_off_at = Instant::now();
    let mut quality_steps = arguments.youtube_qualities.iter().peekable();
    let mut service_page_open = false;
    let mut next_screenshot = Instant::now() + SCREENSHOT_EVERY;
    while started && Instant::now() < deadline {
        if let Some(shots) = &screenshots
            && Instant::now() >= next_screenshot
        {
            shots.take("after-hand-off");
            next_screenshot = Instant::now() + SCREENSHOT_EVERY;
        }
        if !tab_closed && tab_return_at.is_some_and(|at| Instant::now() >= at) {
            tab_closed = true;
            close_tab_and_ask_back(browser.as_mut(), &tab, &control);
        }
        if let Some(step) =
            quality_steps.next_if(|step| Instant::now() >= handed_off_at + step.after)
        {
            ask_youtube_quality(browser.as_mut(), &tab, &mut service_page_open, &step.level);
        }
        if let Some((at, by_seconds)) = seek_at
            && Instant::now() >= at
        {
            seek_at = None;
            match control.seek_by(by_seconds * 1_000_000) {
                Ok(()) => println!("seek by {by_seconds} s sent to the player window"),
                Err(error) => println!("seek by {by_seconds} s failed: {error}"),
            }
        }
        for line in read_new_lines(environment.player_log(), &mut offset) {
            // `FASTIPLAYER_LIVE_ALL_LINES=1`: весь журнал плеера, для разбора.
            if verbose || INTERESTING_LINES.iter().any(|needle| line.contains(needle)) {
                println!("{line}");
            }
            if line.contains("Ad plate") {
                offer_waiting = line.contains(SKIP_OFFER);
            }
        }
        if offer_waiting && arguments.press == Press::Robot {
            match control.next() {
                Ok(()) => println!("robot pressed Next"),
                Err(error) => println!("Next failed: {error}"),
            }
            offer_waiting = false;
        }
        sleep(LOG_POLL);
    }
    browser.shutdown().map_err(|error| error.to_string())?;
    Ok(())
}

/// Возврат вкладки (11b2): пауза в окне, вкладку закрыли, сирота подождала, play в окне. Сбои печатаются:
/// это помощник человека, итог он видит в журнале плеера.
fn close_tab_and_ask_back(
    browser: &mut dyn BrowserUnderTest,
    tab: &TabHandle,
    control: &PlayerControl,
) {
    if let Err(error) = control.pause() {
        println!("tab return: pause failed: {error}");
    }
    sleep(TAB_RETURN_PAUSE);
    // Служебная вкладка — до закрытия: Firefox, закрыв последнюю вкладку, закрыл бы весь браузер.
    if let Err(error) = browser.open_service_page(tab) {
        println!("tab return: the service page did not open: {error}");
    }
    match browser.close_tab(tab.clone()) {
        Ok(()) => println!("tab return: the page tab is closed"),
        Err(error) => println!("tab return: closing the tab failed: {error}"),
    }
    sleep(TAB_RETURN_ORPHAN_WAIT);
    match control.play() {
        Ok(()) => println!("tab return: play pressed in the player window"),
        Err(error) => println!("tab return: play failed: {error}"),
    }
}

fn main() {
    let outcome = parse_arguments().and_then(|arguments| run(&arguments));
    if let Err(message) = outcome {
        eprintln!("live_handoff: {message}");
        std::process::exit(1);
    }
}
