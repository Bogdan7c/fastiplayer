//! Разбор командной строки (вручную, без внешних зависимостей) и справка.

use std::path::{Path, PathBuf};

use browser_bridge_bench::browsers::BrowserKind;
use browser_bridge_bench::environment::{DisplayTarget, PlayerConfigProfile};
use browser_bridge_bench::scenarios::{
    BrowserExecutables, DEFAULT_MIN_FRAMES, PAGE_SCENARIOS, PageFilter, ReplayOptions, RunSettings,
};

mod child_arguments;
mod command_names;
mod job_count;

pub use child_arguments::{ChildRequest, child_arguments};
use command_names::parse_command;
pub use job_count::JobCount;
use job_count::parse_jobs;

/// Что запускать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Pages,
    Smoke,
    Capture,
    PageEnds,
    PlayerCommands,
    ExtensionStatus,
    Codecs,
    Attacks,
    LocalAddress,
    HlsLive,
    LiveEnds,
    HlsLiveSleep,
    PageSubtitles,
    Quality,
    Series,
    SeriesMemory,
    Episodes,
    Entries,
    PageReturn,
    TabReturn,
    PageReload,
    AutoHandoff,
    BlockerMatrix,
    Replay,
    GenerateRecording,
    /// Быстрый набор главных строк (сессия 16b3c2), всегда дочерними процессами.
    Quick,
    All,
}

/// Результат разбора: справка или запуск.
#[derive(Debug)]
pub enum Parsed {
    Help,
    Run(Box<Invocation>),
}

/// Полностью разобранный запуск.
#[derive(Debug)]
pub struct Invocation {
    pub command: Command,
    pub browsers: Vec<BrowserKind>,
    /// Какие страницы гонять (`--page`); без флага — все страницы команды.
    pub pages: PageFilter,
    pub settings: RunSettings,
    /// Параметры `replay` (есть только у этой команды).
    pub replay: Option<ReplayOptions>,
    /// Параметры `generate-recording` (есть только у этой команды).
    pub generation: Option<GenerationOptions>,
    /// Сколько частей прогона идут одновременно (`--jobs`).
    pub jobs: JobCount,
    /// Служебный флаг дочернего процесса параллельного прогона: куда записать строки итога.
    pub results_file: Option<PathBuf>,
}

/// Параметры `generate-recording`: синтетическая запись из ассета стенда.
#[derive(Debug, Clone, PartialEq)]
pub struct GenerationOptions {
    /// Куда записать `*.fprec`.
    pub output: PathBuf,
    /// Ассет каталога медиа (по умолчанию `h264-aac`).
    pub asset: String,
}

impl Command {
    /// Команды, у которых есть список страниц и поэтому работает `--page`.
    fn takes_page_filter(self) -> bool {
        self == Self::All || self.page_ids().is_some()
    }

    /// Находится ли среди страниц команды хоть одна из выбранных: иначе прогон был бы пустым
    /// и «успешным» без единой проверки.
    fn has_any_selected_page(self, filter: &PageFilter) -> bool {
        let pages = || PAGE_SCENARIOS.iter().map(|scenario| scenario.id);
        if let Some(ids) = self.page_ids() {
            return filter.matches_any_of(ids);
        }
        match self {
            // `all` всегда начинается с `pages`, где есть любая известная страница.
            Self::All => filter.matches_any_of(pages()),
            _ => filter.selects_everything(),
        }
    }
}

/// Ошибка использования (код выхода 2).
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct UsageError(String);

/// Корень репозитория: от него считаются пути по умолчанию, чтобы запуск не зависел от cwd.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub const USAGE: &str = "\
Usage: browser-bridge-bench [OPTIONS] <COMMAND>

Commands:
  pages             every stand page in a clean browser (no extension): pass/fail/unsupported
  smoke             one page (mse-fmp4) through the real extension, host and player: the
                    robot hands the tab off, the player presents frames
  capture           every MSE page, same chain; the browser must NOT decode under the shadow
  page-ends         pages that end the hand-off themselves (source withdrawn / replaced by a
                    file / main thread frozen): the player must say so, in time, and close
  player-commands   pause / play / seek / close from the player window reach the page
  extension-status  the real extension (extensions/browser/dist) says \"Connected to Fastiplayer\"
  codecs            the site player asks AV1, then VP9, then H.264: the extension answers for
                    the player (setting x player label, HLS always refused) and the pick plays
  attacks           hostile pages (giant segments, event flood, garbage instead of fMP4,
                    worker message flood): the player must end the session honestly, and a
                    CLEAN page in the SAME browser must then hand off and play
  local-address     a file on a local address under a public-looking page name: refused by
                    default, asks for consent with the 'ask' setting, plays after consent
  hls-live          native HLS LIVE (sliding window, no ENDLIST) under the shadow: the page
                    sees duration = Infinity and a moving seekable window, the player keeps up
                    with the live edge, after closing the player window the element is back
                    at the live edge
  live-ends         a live stream that turns into a recording mid-play (16d): MSE (the page sets a
                    finite duration + endOfStream) and native HLS (#EXT-X-ENDLIST; Chromium only):
                    the player log says 'Live timeline became a recording', frames keep growing,
                    the page sees a finite duration (HLS: exactly one durationchange) and 'ended'
  hls-live-sleep    native HLS live whose server answers 503 for 20 s while the window moves on
                    (16d4; Chromium only): the player waits for the network and rejoins the window
  entries           REAL mouse and keyboard in an invisible kwin: click the extension button on
                    the browser toolbar, Tab / Enter in the popup; entry x page matrix, each
                    row proven by player / host / server logs (needs spectacle and ffmpeg)
  page-return       REAL mouse in an invisible kwin (11a): the \"Playing in Fastiplayer\" placeholder sits
                    over the handed-off video and follows it; \"Return\" makes the page play on from its
                    own second and closes the window silently (4010); closing the window pauses the
                    page; a second tab replaces the first; Return and close at once; the page player
                    works after Return; offset and baked-ad timelines; HLS live edge; a broken page
                    player ends with cause=page_player_failed; Return on a link-entry tab
  tab-return        tab return (11b2), player driven over MPRIS: the source tab is closed while the window
                    is paused, play in the window reopens the page in a new tab and the same session goes
                    on from the same second (MSE and file), the reopened page under the shadow barely
                    decodes; a page showing another video is refused and the window watches the rest;
                    closing the window while the page reopens closes the reopened tab
  auto-handoff      mode 2 \"auto handoff\" (12a), REAL mouse in an invisible kwin: with the mode on, a
                    real click on Play of a film with sound hands it to the player by itself (frames
                    in the player, the browser barely decodes); no hand-off with the mode off, for a
                    muted autoplay banner, a hover preview, a 30 s clip, a site rule \"never\" or the
                    own stop list (the page must really play in the browser); an ad before a film
                    moves to the film; after Return no new hand-off until a new click; three tabs
                    in a row play the last one; live HLS and a stream of unknown length at once
  blocker-matrix    pages x (no blocker / blocker installed first / ours first) with uBlock
                    Origin (Lite); fetch the blockers first with
                    scripts/browser-bridge-bench-fetch-blockers.sh; not part of 'all'
  replay            replay a page-stream recording (--recording FILE.fprec) on the stand page
                    replay-recording with real MSE calls at the original pace, hand the tab
                    off at the recorded 'handoff_began' moment, print the player summaries
                    (presented_frames, position_ms, state) and judge them (--min-frames)
  generate-recording  write a synthetic recording (--output FILE.fprec [--asset ID]) from a
                    stand fMP4 asset: add_source_buffer, paced appends, handoff_began
  page-subtitles    subtitles the page fetches itself (<track>, fetch from another origin, XHR
                    srt + json3, addTextTrack that grows) reach the player: tracks, the selected
                    one, and the subtitle layer on frames
  quality           the page's ABR switches quality inside one SourceBuffer (360p -> 720p) and the
                    player WINDOW shows the new quality (journal 'Browser quality shown'), frames
                    keep growing
  series            the site adapter of the stand's series page: page menus reach the player, a dub
                    and a quality chosen in our menu switch the page's streams, the player returns
                    to the same second, Next opens the next episode in the same dub; a broken
                    adapter leaves plain capture without a menu
  quality           the page's ABR switches quality inside one SourceBuffer (360p -> 720p) and the
                    player WINDOW shows the new quality (journal 'Browser quality shown'), frames
                    keep growing
  series            the site adapter of the stand's series page: page menus reach the player, a dub
                    and a quality chosen in our menu switch the page's streams, the player returns
                    to the same second, Next opens the next episode in the same dub; a broken
                    adapter leaves plain capture without a menu
  series-memory     the extension's choice memory (per site + series): a dub picked on the page
                    is remembered, applied once after a browser restart on the same profile,
                    erased by forget; in a private window the memory is read but never written
  episodes          a gap between episodes (16a): the next episode comes after ~20 s, the window
                    stays fullscreen and waits, hidden tab too; no next episode ends the session
                    after ~60 s; a single-page site that left says 'source tab closed'
  page-reload       a page reloads or navigates inside the same tab (16c): a new video of the same site
                    replaces the session in the same window; no video ends it after ~10 s; another site
                    ends it at once
  quick             the quick set (16b3c2): main rows of content sites (YouTube-like MSE, quality,
                    ad -> film, seeks, <video src>, native HLS, next episode, choice memory,
                    reload, tab return, mode 2) and one feed row per feed device; each row is a
                    child bench process, --jobs at a time
  all               pages, capture, page-ends, player-commands, extension-status, codecs, attacks,
                    local-address, hls-live, live-ends, hls-live-sleep, page-subtitles, quality, series, series-memory, episodes, entries, page-return, tab-return, page-reload and auto-handoff (smoke is a subset of capture), one after another; with --page
                    only the page steps run

Options:
  --browser <chromium|firefox|both>   browsers to test (default: both)
  --display <virtual|real>            invisible kwin (default) or your real screen
  --player-config <clean|gles2-vaapi> player settings inside the bench (default: clean)
  --extra-extension <PATH>            extra extension (unpacked dir, .xpi or .zip) in every
                                      browser launch; repeatable (ad blocker mechanism)
  --page <ID>                         only this page (pages, capture, page-ends,
                                      player-commands, attacks, episodes, entries, page-return, tab-return, page-reload, page-subtitles, auto-handoff, all);
                                      repeatable; ids are the names in site/pages
  --recording <FILE>                  replay: the .fprec file to replay
  --min-frames <N>                    replay: presented frames the player must reach (default: 50)
  --output <FILE>                     generate-recording: where to write the .fprec file
  --asset <ID>                        generate-recording: stand asset id (default: h264-aac)
  --jobs <N>                           parts of the run at once, each a child bench process
                                      with its own site and sandbox (default: processors / 2,
                                      at most 8; 1 with --display real)
  --results-file <FILE>               internal: a child process writes its rows here
  --keep-work                         keep the run directories after the run
  --work-root <DIR>                   run directories (default: target/browser-bridge-bench/work)
  --media-cache <DIR>                 generated media cache (default: target/browser-bridge-bench/media)
  --player-bin-dir <DIR>              directory with fastiplayer and fastiplayer-browser-host
                                      (default: target/debug)
  --browser-path-chromium <PATH>      Chromium executable (default: `chromium` from PATH)
  --browser-path-firefox <PATH>       Firefox executable (default: `firefox` from PATH)
  -h, --help                          show this help

Exit code: 0 all PASS (unsupported is allowed where declared), 1 a failure, 2 usage error.";

/// Разбирает аргументы (без имени программы).
pub fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Parsed, UsageError> {
    let repo = repo_root();
    let mut browsers = vec![BrowserKind::Chromium, BrowserKind::Firefox];
    let mut display = DisplayTarget::Virtual;
    let mut player_config = PlayerConfigProfile::Clean;
    let mut extra_extensions = Vec::new();
    let mut selected_pages: Vec<String> = Vec::new();
    let mut keep_work = false;
    let mut work_root = repo.join("target/browser-bridge-bench/work");
    let mut media_cache = repo.join("target/browser-bridge-bench/media");
    let mut player_bin_dir = repo.join("target/debug");
    let mut executables = BrowserExecutables::default();
    let mut command = None;
    let mut recording: Option<PathBuf> = None;
    let mut min_frames = DEFAULT_MIN_FRAMES;
    let mut output: Option<PathBuf> = None;
    let mut asset = "h264-aac".to_owned();
    let mut jobs: Option<JobCount> = None;
    let mut results_file: Option<PathBuf> = None;

    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let mut value_of = |flag: &str| {
            arguments
                .next()
                .ok_or_else(|| UsageError(format!("{flag} needs a value")))
        };
        match argument.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--browser" => browsers = parse_browsers(&value_of("--browser")?)?,
            "--display" => display = parse_display(&value_of("--display")?)?,
            "--player-config" => {
                player_config = parse_player_config(&value_of("--player-config")?)?
            }
            "--extra-extension" => extra_extensions.push(absolute(value_of("--extra-extension")?)),
            "--page" => selected_pages.push(value_of("--page")?),
            "--recording" => recording = Some(absolute(value_of("--recording")?)),
            "--min-frames" => min_frames = parse_min_frames(&value_of("--min-frames")?)?,
            "--output" => output = Some(absolute(value_of("--output")?)),
            "--asset" => asset = value_of("--asset")?,
            "--keep-work" => keep_work = true,
            "--jobs" => jobs = Some(parse_jobs(&value_of("--jobs")?)?),
            "--results-file" => results_file = Some(absolute(value_of("--results-file")?)),
            "--work-root" => work_root = absolute(value_of("--work-root")?),
            "--media-cache" => media_cache = absolute(value_of("--media-cache")?),
            "--player-bin-dir" => player_bin_dir = absolute(value_of("--player-bin-dir")?),
            "--browser-path-chromium" => {
                executables.chromium = Some(absolute(value_of("--browser-path-chromium")?));
            }
            "--browser-path-firefox" => {
                executables.firefox = Some(absolute(value_of("--browser-path-firefox")?));
            }
            flag if flag.starts_with('-') => {
                return Err(UsageError(format!("unknown option {flag}")));
            }
            name => {
                if command.replace(parse_command(name)?).is_some() {
                    return Err(UsageError("only one command is allowed".to_owned()));
                }
            }
        }
    }
    let command = command.ok_or_else(|| UsageError("a command is required".to_owned()))?;
    let pages = validated_page_filter(command, selected_pages)?;
    let (replay, generation) = command_options(command, recording, min_frames, output, asset)?;
    let jobs = validated_jobs(command, display, jobs)?;
    Ok(Parsed::Run(Box::new(Invocation {
        command,
        browsers,
        pages,
        settings: RunSettings {
            repo_root: repo,
            work_root,
            media_cache,
            player_bin_dir,
            display,
            player_config,
            extra_extensions,
            keep_work,
            browser_executables: executables,
        },
        replay,
        generation,
        jobs,
        results_file,
    })))
}

/// Сколько частей сразу. Настоящий экран один на всех — там только по одной; запись и
/// генерация — одна часть по смыслу. Без флага — по числу процессоров машины.
fn validated_jobs(
    command: Command,
    display: DisplayTarget,
    requested: Option<JobCount>,
) -> Result<JobCount, UsageError> {
    let single_part = display == DisplayTarget::Real
        || matches!(command, Command::Replay | Command::GenerateRecording);
    match requested {
        Some(jobs) if jobs.is_parallel() && single_part => Err(UsageError(format!(
            "--jobs {} needs the virtual display and a command with parts",
            jobs.get()
        ))),
        Some(jobs) => Ok(jobs),
        None if single_part => Ok(JobCount::ONE),
        None => Ok(JobCount::default_for_machine()),
    }
}

/// Параметры, которые бывают только у `replay` и `generate-recording`: недостающий обязательный
/// флаг и флаг не от своей команды — ошибки использования (иначе флаг молча ничего бы не делал).
fn command_options(
    command: Command,
    recording: Option<PathBuf>,
    min_frames: u64,
    output: Option<PathBuf>,
    asset: String,
) -> Result<(Option<ReplayOptions>, Option<GenerationOptions>), UsageError> {
    if recording.is_some() && command != Command::Replay {
        return Err(UsageError(
            "--recording applies only to the replay command".to_owned(),
        ));
    }
    if output.is_some() && command != Command::GenerateRecording {
        return Err(UsageError(
            "--output applies only to the generate-recording command".to_owned(),
        ));
    }
    match command {
        Command::Replay => {
            let recording = recording
                .ok_or_else(|| UsageError("replay needs --recording <FILE.fprec>".to_owned()))?;
            Ok((
                Some(ReplayOptions {
                    recording,
                    min_frames,
                }),
                None,
            ))
        }
        Command::GenerateRecording => {
            let output = output.ok_or_else(|| {
                UsageError("generate-recording needs --output <FILE.fprec>".to_owned())
            })?;
            Ok((None, Some(GenerationOptions { output, asset })))
        }
        _ => Ok((None, None)),
    }
}

fn parse_min_frames(value: &str) -> Result<u64, UsageError> {
    value
        .parse::<u64>()
        .ok()
        .filter(|frames| *frames > 0)
        .ok_or_else(|| {
            UsageError(format!(
                "--min-frames expects a positive integer, got '{value}'"
            ))
        })
}

/// Собирает фильтр страниц и отвергает то, что сделало бы прогон пустым или бессмысленным:
/// неизвестную страницу, `--page` у команды без страниц, выбор, которого нет среди страниц команды.
fn validated_page_filter(
    command: Command,
    selected: Vec<String>,
) -> Result<PageFilter, UsageError> {
    if selected.is_empty() {
        return Ok(PageFilter::all());
    }
    if !command.takes_page_filter() {
        return Err(UsageError(format!(
            "--page does not apply to the {command:?} command"
        )));
    }
    if let Some(unknown) = selected.iter().find(|id| {
        !PAGE_SCENARIOS
            .iter()
            .any(|scenario| scenario.id == id.as_str())
    }) {
        return Err(UsageError(format!(
            "--page: unknown page '{unknown}' (see crates/browser-bridge-bench/site/pages)"
        )));
    }
    let filter = PageFilter::only(selected);
    if !command.has_any_selected_page(&filter) {
        return Err(UsageError(format!(
            "--page: none of {:?} is a page of the {command:?} command",
            filter.ids()
        )));
    }
    Ok(filter)
}

/// Относительные пути пользователя считаются от текущего каталога.
fn absolute(raw: String) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir().map_or(path.clone(), |cwd| cwd.join(&path))
    }
}

fn parse_browsers(value: &str) -> Result<Vec<BrowserKind>, UsageError> {
    match value {
        "chromium" => Ok(vec![BrowserKind::Chromium]),
        "firefox" => Ok(vec![BrowserKind::Firefox]),
        "both" => Ok(vec![BrowserKind::Chromium, BrowserKind::Firefox]),
        other => Err(UsageError(format!(
            "--browser expects chromium, firefox or both, got '{other}'"
        ))),
    }
}

fn parse_display(value: &str) -> Result<DisplayTarget, UsageError> {
    match value {
        "virtual" => Ok(DisplayTarget::Virtual),
        "real" => Ok(DisplayTarget::Real),
        other => Err(UsageError(format!(
            "--display expects virtual or real, got '{other}'"
        ))),
    }
}

fn parse_player_config(value: &str) -> Result<PlayerConfigProfile, UsageError> {
    match value {
        "clean" => Ok(PlayerConfigProfile::Clean),
        "gles2-vaapi" => Ok(PlayerConfigProfile::Gles2Vaapi),
        other => Err(UsageError(format!(
            "--player-config expects clean or gles2-vaapi, got '{other}'"
        ))),
    }
}

#[cfg(test)]
mod tests;
