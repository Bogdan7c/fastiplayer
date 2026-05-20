use std::path::Path;

use player_core::PreparedMedia;
use rustiplayer_config::PlayerDemuxConfig;

/// Audio/container extensions, которые показываем пользователю как быстрые подсказки.
///
/// Это только UI hint для file dialog-а. Реальное определение формата остаётся за
/// Symphonia probe, поэтому файл без расширения или с нестандартным расширением не
/// отсекается на уровне приложения.
pub(crate) const SUPPORTED_LOCAL_MEDIA_EXTENSIONS: &[&str] = &[
    "wav", "aiff", "aif", "caf", "flac", "mp3", "mp2", "mp1", "m4a", "mp4", "ogg", "oga", "opus",
    "alac", "wv", "webm", "mkv",
];

/// Открывает локальный файл через container adapter, который находится вне `player-core`.
pub fn prepare_local_file(
    path: &Path,
    demux_config: &PlayerDemuxConfig,
) -> anyhow::Result<PreparedMedia> {
    let demuxer_options = demuxer_options_from_config(demux_config);
    let demuxer = symphonia_demux::SymphoniaDemuxer::from_file_with_options(path, demuxer_options)?;

    Ok(PreparedMedia::from_local_file(
        path.to_path_buf(),
        Box::new(demuxer),
    ))
}

/// Конвертирует validated TOML config приложения в options Symphonia demux adapter-а.
fn demuxer_options_from_config(config: &PlayerDemuxConfig) -> symphonia_demux::DemuxerOptions {
    symphonia_demux::DemuxerOptions::from_max_consecutive_corrupted_packets(
        config.max_consecutive_corrupted_packets,
    )
    .expect("validated AppConfig must provide positive demux corrupted packet limit")
}

#[cfg(test)]
mod tests {
    use super::SUPPORTED_LOCAL_MEDIA_EXTENSIONS;

    /// Проверяет, что file dialog покрывает audio/container hints, но не превращает их в gate.
    #[test]
    fn local_media_extension_hints_cover_symphonia_audio_containers() {
        let required_extensions = [
            "wav", "aiff", "aif", "caf", "flac", "mp3", "mp2", "mp1", "m4a", "mp4", "ogg", "oga",
            "opus", "alac", "wv", "webm", "mkv",
        ];

        for extension in required_extensions {
            assert!(
                SUPPORTED_LOCAL_MEDIA_EXTENSIONS.contains(&extension),
                "missing local media extension hint: {extension}"
            );
        }

        assert!(
            !SUPPORTED_LOCAL_MEDIA_EXTENSIONS.contains(&"*"),
            "wildcard должен оставаться только в UI filter, а не в списке supported hints"
        );
    }
}
