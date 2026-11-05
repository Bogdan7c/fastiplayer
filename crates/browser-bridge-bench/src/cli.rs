//! Разбор командной строки (вручную, без внешних зависимостей) и справка.

use std::path::{Path, PathBuf};

use browser_bridge_bench::browsers::BrowserKind;
use browser_bridge_bench::environment::{DisplayTarget, PlayerConfigProfile};
use browser_bridge_bench::scenarios::{
    BrowserExecutables, DEFAULT_MIN_FRAMES, PAGE_SCENARIOS, PageFilter, ReplayOptions, RunSettings,
    attack_page_ids, capture_page_ids, end_page_ids, player_command_page_ids,
};

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
    BlockerMatrix,
    Replay,
    GenerateRecording,
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
        matches!(
            self,
            Self::Pages
                | Self::Capture
                | Self::PageEnds
                | Self::PlayerCommands
                | Self::Attacks
                | Self::All
        )
    }

    /// Находится ли среди страниц команды хоть одна из выбранных: иначе прогон был бы пустым
    /// и «успешным» без единой проверки.
    fn has_any_selected_page(self, filter: &PageFilter) -> bool {
        let pages = || PAGE_SCENARIOS.iter().map(|scenario| scenario.id);
        match self {
            Self::Pages => filter.matches_any_of(pages()),
            Self::Capture => filter.matches_any_of(capture_page_ids()),
            Self::PageEnds => filter.matches_any_of(end_page_ids()),
            Self::PlayerCommands => filter.matches_any_of(player_command_page_ids()),
            Self::Attacks => filter.matches_any_of(attack_page_ids()),
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
  blocker-matrix    pages x (no blocker / blocker installed first / ours first) with uBlock
                    Origin (Lite); fetch the blockers first with
                    scripts/browser-bridge-bench-fetch-blockers.sh; not part of 'all'
  replay            replay a page-stream recording (--recording FILE.fprec) on the stand page
                    replay-recording with real MSE calls at the original pace, hand the tab
                    off at the recorded 'handoff_began' moment, print the player summaries
                    (presented_frames, position_ms, state) and judge them (--min-frames)
  generate-recording  write a synthetic recording (--output FILE.fprec [--asset ID]) from a
                    stand fMP4 asset: add_source_buffer, paced appends, handoff_began
  all               pages, capture, page-ends, player-commands, extension-status, codecs, attacks and
                    local-address (smoke is a subset of capture), one after another; with --page
                    only the page steps run

Options:
  --browser <chromium|firefox|both>   browsers to test (default: both)
  --display <virtual|real>            invisible kwin (default) or your real screen
  --player-config <clean|gles2-vaapi> player settings inside the bench (default: clean)
  --extra-extension <PATH>            extra extension (unpacked dir, .xpi or .zip) in every
                                      browser launch; repeatable (ad blocker mechanism)
  --page <ID>                         only this page (pages, capture, page-ends,
                                      player-commands, attacks, all);
                                      repeatable; ids are the names in site/pages
  --recording <FILE>                  replay: the .fprec file to replay
  --min-frames <N>                    replay: presented frames the player must reach (default: 50)
  --output <FILE>                     generate-recording: where to write the .fprec file
  --asset <ID>                        generate-recording: stand asset id (default: h264-aac)
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
    })))
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

fn parse_command(name: &str) -> Result<Command, UsageError> {
    match name {
        "pages" => Ok(Command::Pages),
        "smoke" => Ok(Command::Smoke),
        "capture" => Ok(Command::Capture),
        "page-ends" => Ok(Command::PageEnds),
        "player-commands" => Ok(Command::PlayerCommands),
        "extension-status" => Ok(Command::ExtensionStatus),
        "codecs" => Ok(Command::Codecs),
        "attacks" => Ok(Command::Attacks),
        "local-address" => Ok(Command::LocalAddress),
        "blocker-matrix" => Ok(Command::BlockerMatrix),
        "replay" => Ok(Command::Replay),
        "generate-recording" => Ok(Command::GenerateRecording),
        "all" => Ok(Command::All),
        other => Err(UsageError(format!("unknown command '{other}'"))),
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
mod tests {
    use super::*;

    fn parse_words(words: &[&str]) -> Result<Parsed, UsageError> {
        parse(words.iter().map(|word| (*word).to_owned()))
    }

    fn invocation(words: &[&str]) -> Invocation {
        match parse_words(words).expect("parses") {
            Parsed::Run(invocation) => *invocation,
            Parsed::Help => panic!("expected a run"),
        }
    }

    #[test]
    fn defaults_are_both_browsers_virtual_display_clean_config() {
        let run = invocation(&["smoke"]);
        assert_eq!(run.command, Command::Smoke);
        assert_eq!(
            run.browsers,
            vec![BrowserKind::Chromium, BrowserKind::Firefox]
        );
        assert_eq!(run.settings.display, DisplayTarget::Virtual);
        assert_eq!(run.settings.player_config, PlayerConfigProfile::Clean);
        assert!(
            run.settings
                .work_root
                .ends_with("target/browser-bridge-bench/work")
        );
        assert!(run.settings.player_bin_dir.ends_with("target/debug"));
    }

    #[test]
    fn flags_may_surround_the_command_and_repeat() {
        let run = invocation(&[
            "--browser",
            "firefox",
            "--display",
            "real",
            "--player-config",
            "gles2-vaapi",
            "--extra-extension",
            "/a",
            "all",
            "--extra-extension",
            "/b",
            "--keep-work",
        ]);
        assert_eq!(run.command, Command::All);
        assert_eq!(run.browsers, vec![BrowserKind::Firefox]);
        assert_eq!(run.settings.display, DisplayTarget::Real);
        assert_eq!(run.settings.player_config, PlayerConfigProfile::Gles2Vaapi);
        assert_eq!(
            run.settings.extra_extensions,
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
        assert!(run.settings.keep_work);
    }

    #[test]
    fn new_capture_commands_are_recognised() {
        assert_eq!(invocation(&["capture"]).command, Command::Capture);
        assert_eq!(
            invocation(&["player-commands"]).command,
            Command::PlayerCommands
        );
    }

    #[test]
    fn page_flag_repeats_and_builds_a_filter() {
        let run = invocation(&["capture", "--page", "page-freeze", "--page", "mse-fmp4"]);
        // page-freeze не страница capture, но mse-fmp4 — да: выбор пересекается.
        assert_eq!(run.pages.ids(), ["page-freeze", "mse-fmp4"]);
        assert!(run.pages.admits("mse-fmp4") && !run.pages.admits("mse-webm"));
        assert!(invocation(&["capture"]).pages.selects_everything());
        let ends = invocation(&[
            "page-ends",
            "--page",
            "source-withdrawn",
            "--page",
            "source-withdrawn",
        ]);
        assert_eq!(ends.command, Command::PageEnds);
        assert_eq!(ends.pages.ids(), ["source-withdrawn"]);
    }

    #[test]
    fn page_flag_rejects_unknown_pages_foreign_pages_and_commands_without_pages() {
        let cases: [(&[&str], &str); 6] = [
            (
                &["capture", "--page", "no-such-page"],
                "unknown page 'no-such-page'",
            ),
            (
                &["player-commands", "--page", "page-freeze"],
                "is a page of the PlayerCommands",
            ),
            (
                &["capture", "--page", "page-freeze"],
                "none of [\"page-freeze\"] is a page of the Capture",
            ),
            (
                &["page-ends", "--page", "mse-fmp4"],
                "is a page of the PageEnds",
            ),
            (
                &["smoke", "--page", "mse-fmp4"],
                "--page does not apply to the Smoke",
            ),
            (&["capture", "--page"], "--page needs a value"),
        ];
        for (words, expected) in cases {
            let error = parse_words(words).expect_err("must be rejected");
            assert!(error.to_string().contains(expected), "{words:?}: {error}");
        }
    }

    #[test]
    fn player_commands_take_a_page_filter_for_the_live_stream_pages() {
        let run = invocation(&["player-commands", "--page", "live-unload-worker"]);
        assert_eq!(run.command, Command::PlayerCommands);
        assert!(run.pages.admits("live-unload-worker") && !run.pages.admits("mse-fmp4"));
        assert!(invocation(&["player-commands"]).pages.selects_everything());
    }

    #[test]
    fn page_flag_works_for_pages_and_all_with_any_known_page() {
        assert_eq!(invocation(&["pages", "--page", "pip"]).pages.ids(), ["pip"]);
        let all = invocation(&["all", "--page", "page-freeze"]);
        assert_eq!(all.command, Command::All);
        assert_eq!(all.pages.ids(), ["page-freeze"]);
    }

    #[test]
    fn the_09c_commands_are_recognised_and_only_attacks_take_pages() {
        assert_eq!(invocation(&["attacks"]).command, Command::Attacks);
        assert_eq!(
            invocation(&["local-address"]).command,
            Command::LocalAddress
        );
        assert_eq!(
            invocation(&["blocker-matrix"]).command,
            Command::BlockerMatrix
        );
        let one = invocation(&["attacks", "--page", "attack-garbage"]);
        assert_eq!(one.pages.ids(), ["attack-garbage"]);
        for words in [
            &["local-address", "--page", "video-src-local"][..],
            &["blocker-matrix", "--page", "mse-fmp4"][..],
            &["attacks", "--page", "mse-fmp4"][..],
        ] {
            assert!(parse_words(words).is_err(), "{words:?} must be rejected");
        }
    }

    #[test]
    fn page_ends_command_is_recognised() {
        assert_eq!(invocation(&["page-ends"]).command, Command::PageEnds);
    }

    #[test]
    fn replay_needs_a_recording_and_reads_min_frames() {
        let run = invocation(&[
            "replay",
            "--recording",
            "/tmp/a.fprec",
            "--browser",
            "firefox",
            "--min-frames",
            "120",
        ]);
        assert_eq!(run.command, Command::Replay);
        let replay = run.replay.expect("replay options");
        assert_eq!(replay.recording, PathBuf::from("/tmp/a.fprec"));
        assert_eq!(replay.min_frames, 120);
        assert_eq!(
            invocation(&["replay", "--recording", "/tmp/a.fprec"])
                .replay
                .map(|o| o.min_frames),
            Some(DEFAULT_MIN_FRAMES)
        );
        let cases: [(&[&str], &str); 4] = [
            (&["replay"], "replay needs --recording"),
            (
                &["smoke", "--recording", "/x"],
                "--recording applies only to the replay",
            ),
            (
                &["replay", "--recording", "/x", "--min-frames", "0"],
                "positive integer",
            ),
            (
                &["replay", "--recording", "/x", "--page", "mse-fmp4"],
                "--page does not apply",
            ),
        ];
        for (words, expected) in cases {
            let error = parse_words(words).expect_err("must be rejected");
            assert!(error.to_string().contains(expected), "{words:?}: {error}");
        }
    }

    #[test]
    fn generate_recording_needs_an_output_and_defaults_the_asset() {
        let run = invocation(&["generate-recording", "--output", "/tmp/s.fprec"]);
        assert_eq!(
            run.generation,
            Some(GenerationOptions {
                output: PathBuf::from("/tmp/s.fprec"),
                asset: "h264-aac".into()
            })
        );
        assert!(parse_words(&["generate-recording"]).is_err());
        assert!(parse_words(&["smoke", "--output", "/x"]).is_err());
    }

    #[test]
    fn help_wins_over_everything() {
        assert!(matches!(parse_words(&["--help"]), Ok(Parsed::Help)));
        assert!(matches!(parse_words(&["smoke", "-h"]), Ok(Parsed::Help)));
    }

    #[test]
    fn usage_errors_are_reported_with_the_offending_value() {
        let cases: [(&[&str], &str); 6] = [
            (&[], "a command is required"),
            (&["smoke", "pages"], "only one command"),
            (&["bogus"], "unknown command 'bogus'"),
            (&["--nope", "smoke"], "unknown option --nope"),
            (&["--browser"], "--browser needs a value"),
            (&["--browser", "safari", "smoke"], "got 'safari'"),
        ];
        for (words, expected) in cases {
            let error = parse_words(words).expect_err("must be rejected");
            assert!(error.to_string().contains(expected), "{words:?}: {error}");
        }
    }
}
