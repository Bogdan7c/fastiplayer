use std::num::NonZeroUsize;
use std::path::Path;
use std::time::Duration;

use demux_api::{
    DemuxFactoryOpenError, DemuxHints, DemuxInput, DemuxOpenError, DemuxProbeRejection,
    DemuxRegistry, DemuxSniffBudget, DemuxSourceExtension,
};
use fastiplayer_config::PlayerDemuxConfig;
use media_core::Demuxer;
use player_core::PreparedMedia;
use source_core::{CancellationToken, LocalFileSource};

/// Audio/container extensions, которые показываем пользователю как быстрые подсказки.
///
/// Это только UI hint для file dialog-а. Реальное определение формата остаётся за
/// Symphonia probe, поэтому файл без расширения или с нестандартным расширением не
/// отсекается на уровне приложения.
pub const SUPPORTED_LOCAL_MEDIA_EXTENSIONS: &[&str] = &[
    "wav", "aiff", "aif", "caf", "flac", "mp3", "mp2", "mp1", "m4a", "mp4", "ogg", "oga", "opus",
    "alac", "wv", "webm", "mkv", "mov", "ts",
];

/// Похож ли путь на media-файл по расширению из [`SUPPORTED_LOCAL_MEDIA_EXTENSIONS`].
///
/// Единственное правило «media-кандидата» для массового отбора файлов (например,
/// рекурсивный обход брошенной папки): регистр расширения не важен, файл без расширения
/// не считается кандидатом. Формат окончательно определяет обычный probe при открытии.
#[must_use]
pub fn has_supported_local_media_extension(path: &Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| {
            SUPPORTED_LOCAL_MEDIA_EXTENSIONS
                .iter()
                .any(|supported| supported.eq_ignore_ascii_case(extension))
        })
}

/// Local composition сохраняет typed registry cancellation до media-open boundary.
#[derive(Debug, thiserror::Error)]
pub enum LocalDemuxOpenError {
    /// Ошибка static registry/factory composition, не зависящая от media bytes.
    #[error("не удалось собрать local demux registry")]
    RegistrySetup(#[source] anyhow::Error),
    /// Typed probe/open outcome конкретного уже открытого source-а.
    #[error("local demux registry не открыл media source")]
    Open(#[source] DemuxOpenError),
}

impl LocalDemuxOpenError {
    /// Cancellation остаётся отдельным outcome, а не текстом внутри anyhow chain.
    pub fn is_cancelled(&self) -> bool {
        matches!(
            self,
            Self::Open(DemuxOpenError::ProbeRejected(
                DemuxProbeRejection::Cancelled
            )) | Self::Open(DemuxOpenError::FactoryRejected {
                source: DemuxFactoryOpenError::Cancelled,
                ..
            })
        )
    }
}

/// Открывает локальный файл через container adapter, который находится вне `player-core`.
pub fn prepare_local_file(
    path: &Path,
    demux_config: &PlayerDemuxConfig,
) -> anyhow::Result<PreparedMedia> {
    let demuxer = open_local_demuxer(path, demux_config)?;

    Ok(PreparedMedia::from_local_file(path.to_path_buf(), demuxer))
}

/// Открывает новый local demuxer вне `player-core`.
///
/// Каждый вызов создаёт свой demuxer, поэтому независимые потребители не
/// двигают playback demuxer и не делят с ним cursor.
pub(crate) fn open_local_demuxer(
    path: &Path,
    demux_config: &PlayerDemuxConfig,
) -> anyhow::Result<Box<dyn Demuxer + Send>> {
    let local_source = LocalFileSource::open(path)?;
    let extension_hint = path.extension().and_then(|value| value.to_str());
    Ok(open_local_demuxer_from_source(
        local_source,
        extension_hint,
        demux_config,
        CancellationToken::never_cancelled(),
    )?)
}

/// Композирует все local-capable demux factories над уже открытым source handle-ом.
pub fn open_local_demuxer_from_source(
    local_source: LocalFileSource,
    extension_hint: Option<&str>,
    demux_config: &PlayerDemuxConfig,
    cancellation: CancellationToken,
) -> Result<Box<dyn Demuxer + Send>, LocalDemuxOpenError> {
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(
            symphonia_demux::SymphoniaDemuxFactory::new(demuxer_options_from_config(demux_config))
                .map_err(|error| LocalDemuxOpenError::RegistrySetup(error.into()))?,
        ))
        .map_err(|error| LocalDemuxOpenError::RegistrySetup(error.into()))?;
    registry
        .register(Box::new(
            mpeg_ts_demux::MpegTsDemuxFactory::new(mpeg_ts_demux::MpegTsDemuxOptions::default())
                .map_err(|error| LocalDemuxOpenError::RegistrySetup(error.into()))?,
        ))
        .map_err(|error| LocalDemuxOpenError::RegistrySetup(error.into()))?;
    let hints = extension_hint
        .map(str::to_ascii_lowercase)
        .and_then(|extension| DemuxSourceExtension::new(extension).ok())
        .map_or_else(DemuxHints::none, |extension| {
            DemuxHints::none().with_extension(extension)
        });
    registry
        .open(
            DemuxInput::byte_source(Box::new(local_source)),
            hints,
            local_demux_sniff_budget(),
            cancellation,
        )
        .map_err(LocalDemuxOpenError::Open)
}

/// Named local sniff policy удерживает I/O/replay независимо от container factory.
#[expect(
    clippy::expect_used,
    reason = "DemuxSniffBudget::new отклоняет только нулевую длительность, а здесь константа 2 с"
)]
fn local_demux_sniff_budget() -> DemuxSniffBudget {
    DemuxSniffBudget::new(
        const { NonZeroUsize::new(256 * 1024).expect("local sniff byte limit is non-zero") },
        const { NonZeroUsize::new(8).expect("local sniff segment limit is non-zero") },
        Duration::from_secs(2),
    )
    .expect("local sniff duration is non-zero")
}

/// Конвертирует validated TOML config приложения в options Symphonia demux adapter-а.
#[expect(
    clippy::expect_used,
    reason = "инвариант: validated AppConfig must provide positive demux corrupted packet limit"
)]
pub(crate) fn demuxer_options_from_config(
    config: &PlayerDemuxConfig,
) -> symphonia_demux::DemuxerOptions {
    symphonia_demux::DemuxerOptions::from_max_consecutive_corrupted_packets(
        config.max_consecutive_corrupted_packets,
    )
    .expect("validated AppConfig must provide positive demux corrupted packet limit")
}

#[cfg(test)]
mod ts_fixture;

#[cfg(test)]
mod tests {
    use media_core::VideoPacketFraming;

    use super::{
        SUPPORTED_LOCAL_MEDIA_EXTENSIONS, has_supported_local_media_extension, open_local_demuxer,
        open_local_demuxer_from_source, prepare_local_file,
    };

    /// Кандидат определяется расширением без учёта регистра; прочее отсекается.
    #[test]
    fn media_candidate_rule_is_case_insensitive_and_rejects_other_files() {
        use std::path::Path;

        assert!(has_supported_local_media_extension(Path::new(
            "/m/Film.MKV"
        )));
        assert!(has_supported_local_media_extension(Path::new("song.flac")));
        assert!(!has_supported_local_media_extension(Path::new("cover.jpg")));
        assert!(!has_supported_local_media_extension(Path::new(
            "playlist.m3u"
        )));
        assert!(!has_supported_local_media_extension(Path::new(
            "no_extension"
        )));
    }

    /// Проверяет, что file dialog покрывает audio/container hints, но не превращает их в gate.
    #[test]
    fn local_media_extension_hints_cover_symphonia_audio_containers() {
        let required_extensions = [
            "wav", "aiff", "aif", "caf", "flac", "mp3", "mp2", "mp1", "m4a", "mp4", "ogg", "oga",
            "opus", "alac", "wv", "webm", "mkv", "mov", "ts",
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

    #[test]
    fn prepare_and_rebuild_open_generated_local_ts_with_annex_b_evidence() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("generated.ts");
        std::fs::write(&path, super::ts_fixture::mpeg_ts_h264_aac_bytes())
            .expect("write generated TS");
        let config = fastiplayer_config::PlayerDemuxConfig::default();

        let prepared = prepare_local_file(&path, &config).expect("prepare local TS");
        let rebuilt = open_local_demuxer(&path, &config).expect("rebuild local TS demuxer");

        for tracks in [prepared.tracks(), rebuilt.tracks()] {
            let video = tracks
                .iter()
                .find(|track| track.kind == media_core::TrackKind::Video)
                .expect("video track");
            assert_eq!(
                video.video.as_ref().expect("video metadata").packet_framing,
                VideoPacketFraming::AnnexB
            );
        }
    }

    #[test]
    fn registry_probe_cancellation_remains_typed_at_local_boundary() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("cancel-probe.ts");
        std::fs::write(&path, super::ts_fixture::mpeg_ts_h264_aac_bytes()).expect("write TS");
        let cancellation = source_core::CancellationToken::new();
        cancellation.cancel();
        let source = source_core::LocalFileSource::open(&path).expect("open one handle");

        let error = match open_local_demuxer_from_source(
            source,
            Some("ts"),
            &fastiplayer_config::PlayerDemuxConfig::default(),
            cancellation,
        ) {
            Ok(_) => panic!("cancelled registry probe must not return demuxer"),
            Err(error) => error,
        };

        assert!(error.is_cancelled());
    }
}
