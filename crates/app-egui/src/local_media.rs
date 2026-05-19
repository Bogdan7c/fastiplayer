use std::path::Path;

use player_core::PreparedMedia;
use rustiplayer_config::PlayerDemuxConfig;

/// Открывает локальный файл через container adapter, который находится вне `player-core`.
pub fn prepare_local_file(
    path: &Path,
    demux_config: &PlayerDemuxConfig,
) -> anyhow::Result<PreparedMedia> {
    let demuxer_options = demuxer_options_from_config(demux_config);
    let demuxer = webm_demux::SymphoniaDemuxer::from_file_with_options(path, demuxer_options)?;

    Ok(PreparedMedia::from_local_file(
        path.to_path_buf(),
        Box::new(demuxer),
    ))
}

/// Конвертирует validated TOML config приложения в backend-specific WebM options.
fn demuxer_options_from_config(config: &PlayerDemuxConfig) -> webm_demux::DemuxerOptions {
    webm_demux::DemuxerOptions::from_max_consecutive_corrupted_packets(
        config.max_consecutive_corrupted_packets,
    )
    .expect("validated AppConfig must provide positive demux corrupted packet limit")
}
