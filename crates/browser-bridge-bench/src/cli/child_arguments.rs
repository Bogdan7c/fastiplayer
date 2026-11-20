//! Командная строка дочернего процесса стенда (параллельный прогон, сессия 16b3c2).
//!
//! Родитель гоняет каждую часть прогона (команда × браузер × страница) отдельным процессом
//! того же стенда — со своим сайтом, журналом запросов сервера и песочницей. Чтобы ребёнок
//! видел ровно те же настройки, что родитель, родитель собирает его аргументы из уже
//! разобранных [`RunSettings`] теми же флагами, которые принимает [`super::parse`]; тест
//! «туда и обратно» держит обе стороны вместе.

use std::path::Path;

use browser_bridge_bench::browsers::BrowserKind;
use browser_bridge_bench::environment::{DisplayTarget, PlayerConfigProfile};
use browser_bridge_bench::scenarios::RunSettings;

use super::{Command, UsageError};

/// Что гонять в дочернем процессе.
#[derive(Debug, Clone, Copy)]
pub struct ChildRequest<'a> {
    /// Команда части (никогда не `all` и не `quick`: они раскладываются на части).
    pub command: Command,
    /// Один браузер.
    pub browser: BrowserKind,
    /// Одна страница команды (`None` — команда без списка страниц, целиком).
    pub page: Option<&'static str>,
    /// Куда ребёнку записать строки результата.
    pub results_file: &'a Path,
}

/// Аргументы дочернего процесса (без имени программы). Ошибка — настройку нельзя передать
/// командной строкой (путь не UTF-8, профиль плеера только для сценариев).
pub fn child_arguments(
    request: &ChildRequest<'_>,
    settings: &RunSettings,
) -> Result<Vec<String>, UsageError> {
    let mut arguments = vec![
        request.command.cli_name().to_owned(),
        "--browser".to_owned(),
        request.browser.catalog_name().to_owned(),
        // Ребёнок гоняет свою часть по очереди сам: дальше не делится.
        "--jobs".to_owned(),
        "1".to_owned(),
        "--results-file".to_owned(),
        path_text("--results-file", request.results_file)?,
    ];
    if let Some(page) = request.page {
        arguments.extend(["--page".to_owned(), page.to_owned()]);
    }
    arguments.extend([
        "--display".to_owned(),
        display_name(settings.display).to_owned(),
        "--player-config".to_owned(),
        player_config_name(settings.player_config)?.to_owned(),
    ]);
    for extension in &settings.extra_extensions {
        arguments.extend([
            "--extra-extension".to_owned(),
            path_text("--extra-extension", extension)?,
        ]);
    }
    if settings.keep_work {
        arguments.push("--keep-work".to_owned());
    }
    for (flag, path) in [
        ("--work-root", &settings.work_root),
        ("--media-cache", &settings.media_cache),
        ("--player-bin-dir", &settings.player_bin_dir),
    ] {
        arguments.extend([flag.to_owned(), path_text(flag, path)?]);
    }
    for (flag, path) in [
        (
            "--browser-path-chromium",
            &settings.browser_executables.chromium,
        ),
        (
            "--browser-path-firefox",
            &settings.browser_executables.firefox,
        ),
    ] {
        if let Some(path) = path {
            arguments.extend([flag.to_owned(), path_text(flag, path)?]);
        }
    }
    Ok(arguments)
}

/// Имя экрана для `--display`.
fn display_name(display: DisplayTarget) -> &'static str {
    match display {
        DisplayTarget::Virtual => "virtual",
        DisplayTarget::Real => "real",
    }
}

/// Имя профиля для `--player-config`; профили «только для сценариев» в командной строке не
/// бывают, и родитель с таким профилем — ошибка, а не молчаливая подмена.
fn player_config_name(profile: PlayerConfigProfile) -> Result<&'static str, UsageError> {
    match profile {
        PlayerConfigProfile::Clean => Ok("clean"),
        PlayerConfigProfile::Gles2Vaapi => Ok("gles2-vaapi"),
        other => Err(UsageError(format!(
            "the player profile {other:?} cannot be passed to a child bench process"
        ))),
    }
}

/// Путь как текст аргумента; не UTF-8 — ошибка (разбор принимает только `String`).
fn path_text(flag: &str, path: &Path) -> Result<String, UsageError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| UsageError(format!("{flag}: the path {path:?} is not UTF-8")))
}
