//! Разбор командной строки (вручную, без внешних зависимостей) и справка.

use std::path::{Path, PathBuf};

use browser_bridge_bench::browsers::BrowserKind;
use browser_bridge_bench::environment::{DisplayTarget, PlayerConfigProfile};
use browser_bridge_bench::scenarios::{BrowserExecutables, RunSettings};

/// Что запускать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Pages,
    Smoke,
    ExtensionStatus,
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
    pub settings: RunSettings,
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
  smoke             stand interceptor + real host + real player: frames reach the renderer
  extension-status  the real extension (extensions/browser/dist) says \"Connected to Fastiplayer\"
  all               pages, smoke and extension-status, one after another

Options:
  --browser <chromium|firefox|both>   browsers to test (default: both)
  --display <virtual|real>            invisible kwin (default) or your real screen
  --player-config <clean|gles2-vaapi> player settings inside the bench (default: clean)
  --extra-extension <PATH>            extra extension (unpacked dir, .xpi or .zip) in every
                                      browser launch; repeatable (ad blocker mechanism)
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
    let mut keep_work = false;
    let mut work_root = repo.join("target/browser-bridge-bench/work");
    let mut media_cache = repo.join("target/browser-bridge-bench/media");
    let mut player_bin_dir = repo.join("target/debug");
    let mut executables = BrowserExecutables::default();
    let mut command = None;

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
    Ok(Parsed::Run(Box::new(Invocation {
        command,
        browsers,
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
    })))
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
        "extension-status" => Ok(Command::ExtensionStatus),
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
