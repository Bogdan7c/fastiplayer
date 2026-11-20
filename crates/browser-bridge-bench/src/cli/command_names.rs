//! Имена команд в обе стороны и списки страниц команд.
//!
//! Параллельный прогон (сессия 16b3c2) запускает дочерние процессы стенда той же командной
//! строкой, что и человек, поэтому разбор имени и обратное имя живут рядом и проверяются
//! одним тестом «туда и обратно».

use browser_bridge_bench::scenarios::{
    PAGE_SCENARIOS, attack_page_ids, auto_handoff_page_ids, capture_page_ids, end_page_ids,
    entries_page_ids, episodes_page_ids, page_reload_page_ids, page_return_page_ids,
    page_subtitles_page_ids, player_command_page_ids, quality_page_ids, series_memory_page_ids,
    series_page_ids, tab_return_page_ids,
};

use super::{Command, UsageError};

/// Все команды — для теста «имя туда и обратно» и справки.
#[cfg(test)]
pub(super) const ALL_COMMANDS: [Command; 27] = [
    Command::Pages,
    Command::Smoke,
    Command::Capture,
    Command::PageEnds,
    Command::PlayerCommands,
    Command::ExtensionStatus,
    Command::Codecs,
    Command::Attacks,
    Command::LocalAddress,
    Command::HlsLive,
    Command::LiveEnds,
    Command::HlsLiveSleep,
    Command::PageSubtitles,
    Command::Quality,
    Command::Series,
    Command::SeriesMemory,
    Command::Episodes,
    Command::Entries,
    Command::PageReturn,
    Command::TabReturn,
    Command::PageReload,
    Command::AutoHandoff,
    Command::BlockerMatrix,
    Command::Replay,
    Command::GenerateRecording,
    Command::Quick,
    Command::All,
];

impl Command {
    /// Имя команды в командной строке.
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::Pages => "pages",
            Self::Smoke => "smoke",
            Self::Capture => "capture",
            Self::PageEnds => "page-ends",
            Self::PlayerCommands => "player-commands",
            Self::ExtensionStatus => "extension-status",
            Self::Codecs => "codecs",
            Self::Attacks => "attacks",
            Self::LocalAddress => "local-address",
            Self::HlsLive => "hls-live",
            Self::LiveEnds => "live-ends",
            Self::HlsLiveSleep => "hls-live-sleep",
            Self::PageSubtitles => "page-subtitles",
            Self::Quality => "quality",
            Self::Series => "series",
            Self::SeriesMemory => "series-memory",
            Self::Episodes => "episodes",
            Self::Entries => "entries",
            Self::PageReturn => "page-return",
            Self::TabReturn => "tab-return",
            Self::PageReload => "page-reload",
            Self::AutoHandoff => "auto-handoff",
            Self::BlockerMatrix => "blocker-matrix",
            Self::Replay => "replay",
            Self::GenerateRecording => "generate-recording",
            Self::Quick => "quick",
            Self::All => "all",
        }
    }

    /// Страницы команды, у которой работает `--page`; `None` — у команды нет списка страниц
    /// (она гоняется целиком). У `all` своего списка нет: он сборный.
    pub fn page_ids(self) -> Option<Vec<&'static str>> {
        let ids = match self {
            Self::Pages => PAGE_SCENARIOS.iter().map(|scenario| scenario.id).collect(),
            Self::Capture => capture_page_ids(),
            Self::PageEnds => end_page_ids(),
            Self::PlayerCommands => player_command_page_ids(),
            Self::Attacks => attack_page_ids(),
            Self::PageSubtitles => page_subtitles_page_ids(),
            Self::Quality => quality_page_ids(),
            Self::Series => series_page_ids(),
            Self::SeriesMemory => series_memory_page_ids(),
            Self::Episodes => episodes_page_ids(),
            Self::Entries => entries_page_ids(),
            Self::PageReturn => page_return_page_ids(),
            Self::TabReturn => tab_return_page_ids(),
            Self::PageReload => page_reload_page_ids(),
            Self::AutoHandoff => auto_handoff_page_ids(),
            _ => return None,
        };
        Some(ids)
    }
}

/// Имя из командной строки → команда.
pub(super) fn parse_command(name: &str) -> Result<Command, UsageError> {
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
        "hls-live" => Ok(Command::HlsLive),
        "live-ends" => Ok(Command::LiveEnds),
        "hls-live-sleep" => Ok(Command::HlsLiveSleep),
        "page-subtitles" => Ok(Command::PageSubtitles),
        "quality" => Ok(Command::Quality),
        "series" => Ok(Command::Series),
        "series-memory" => Ok(Command::SeriesMemory),
        "episodes" => Ok(Command::Episodes),
        "entries" => Ok(Command::Entries),
        "page-return" => Ok(Command::PageReturn),
        "tab-return" => Ok(Command::TabReturn),
        "page-reload" => Ok(Command::PageReload),
        "auto-handoff" => Ok(Command::AutoHandoff),
        "blocker-matrix" => Ok(Command::BlockerMatrix),
        "replay" => Ok(Command::Replay),
        "generate-recording" => Ok(Command::GenerateRecording),
        "quick" => Ok(Command::Quick),
        "all" => Ok(Command::All),
        other => Err(UsageError(format!("unknown command '{other}'"))),
    }
}
