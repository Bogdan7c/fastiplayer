use std::fmt;
use std::path::{Path, PathBuf};

use media_core::Demuxer;
use rustiplayer_config::{NetworkConfig, PlayerDemuxConfig, YoutubeConfig};
use service_direct_media::DirectMediaOpenError;
use service_youtube::YoutubeSeekableVodOpenError;

use crate::local_media;

/// App-owned identity источника, который hover executor может открыть отдельно от playback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TimelineHoverSourceIdentity {
    /// Local file переоткрывается через тот же app-level helper, что и playback local open.
    LocalFile(PathBuf),

    /// Direct URL переоткрывается через service-direct-media seekable Range policy.
    DirectMediaUrl(String),

    /// YouTube VOD переоткрывается через compact selected stream identity.
    YouTubeUrl {
        /// Исходный YouTube page URL.
        source_url: String,

        /// Выбранная stream-пара без хранения полного candidates manifest-а.
        selected_stream_identity: service_youtube::YoutubeSelectedStreamIdentity,
    },
}

/// Source kind для typed unsupported outcome-ов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverUnsupportedSourceKind {
    DirectMediaUrl,
    YouTubeUrl,
}

/// Source kind для typed open-failed outcome-ов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverOpenFailedSourceKind {
    LocalFile,
    DirectMediaUrl,
    YouTubeUrl,
}

/// Результат попытки получить независимый hover source.
pub(crate) enum TimelineHoverSourceOpenOutcome {
    /// Source успешно открыт и принадлежит только hover executor-у.
    Opened(TimelineHoverOpenedSource),

    /// App ещё не сообщил executor-у текущий media source или source был invalidated.
    MissingActiveSource,

    /// Source известен, но этот тип ещё не поддержан hover factory в текущей сессии.
    Unsupported {
        source_kind: TimelineHoverUnsupportedSourceKind,
    },

    /// Source поддержан, но открыть новый independent demuxer не удалось.
    OpenFailed {
        source_kind: TimelineHoverOpenFailedSourceKind,
    },
}

impl fmt::Debug for TimelineHoverSourceOpenOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Opened(_source) => formatter.write_str("Opened(TimelineHoverOpenedSource)"),
            Self::MissingActiveSource => formatter.write_str("MissingActiveSource"),
            Self::Unsupported { source_kind } => formatter
                .debug_struct("Unsupported")
                .field("source_kind", source_kind)
                .finish(),
            Self::OpenFailed { source_kind } => formatter
                .debug_struct("OpenFailed")
                .field("source_kind", source_kind)
                .finish(),
        }
    }
}

/// Owned hover source, который не связан с playback demuxer lifecycle.
pub(crate) struct TimelineHoverOpenedSource {
    demuxer: Box<dyn Demuxer + Send>,
}

impl TimelineHoverOpenedSource {
    /// Создаёт owned hover source из demuxer-а, который не связан с playback.
    #[must_use]
    pub(crate) fn from_demuxer(demuxer: Box<dyn Demuxer + Send>) -> Self {
        Self { demuxer }
    }

    /// Даёт executor-у mutating доступ только к independent hover demuxer-у.
    pub(crate) fn demuxer_mut(&mut self) -> &mut (dyn Demuxer + Send) {
        self.demuxer.as_mut()
    }

    /// Передаёт demuxer будущему hover decode executor-у.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn into_demuxer(self) -> Box<dyn Demuxer + Send> {
        self.demuxer
    }
}

/// Factory хранит только app-level source identity и demux config для новых hover opens.
#[derive(Clone)]
pub(crate) struct TimelineHoverSourceFactory {
    active_source: Option<TimelineHoverSourceIdentity>,
    network_config: NetworkConfig,
    youtube_config: YoutubeConfig,
    demux_config: PlayerDemuxConfig,
}

impl TimelineHoverSourceFactory {
    /// Создаёт factory без active source; source приходит позже из media open lifecycle.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn new(demux_config: PlayerDemuxConfig) -> Self {
        Self::with_open_config(
            NetworkConfig::default(),
            YoutubeConfig::default(),
            demux_config,
        )
    }

    /// Создаёт factory с committed config snapshot-ом для новых source opens.
    #[must_use]
    pub(crate) fn with_open_config(
        network_config: NetworkConfig,
        youtube_config: YoutubeConfig,
        demux_config: PlayerDemuxConfig,
    ) -> Self {
        Self {
            active_source: None,
            network_config,
            youtube_config,
            demux_config,
        }
    }

    /// Обновляет committed config snapshot для следующих hover opens.
    pub(crate) fn update_open_config(
        &mut self,
        network_config: NetworkConfig,
        youtube_config: YoutubeConfig,
        demux_config: PlayerDemuxConfig,
    ) {
        self.network_config = network_config;
        self.youtube_config = youtube_config;
        self.demux_config = demux_config;
    }

    /// Запоминает новый source context и тем самым заменяет старый.
    pub(crate) fn set_active_source(&mut self, active_source: TimelineHoverSourceIdentity) {
        self.active_source = Some(active_source);
    }

    /// Сбрасывает source context при media switch до успешного открытия нового source.
    pub(crate) fn invalidate_active_source(&mut self) {
        self.active_source = None;
    }

    /// Возвращает `true`, если текущий source нужно открывать только на background thread-е.
    #[must_use]
    pub(crate) fn active_source_is_network(&self) -> bool {
        matches!(
            self.active_source.as_ref(),
            Some(
                TimelineHoverSourceIdentity::DirectMediaUrl(_)
                    | TimelineHoverSourceIdentity::YouTubeUrl { .. }
            )
        )
    }

    /// Возвращает identity только для sources, которые hover открывает на background thread-е.
    #[must_use]
    pub(crate) fn active_network_source_identity(&self) -> Option<TimelineHoverSourceIdentity> {
        match self.active_source.as_ref()? {
            active_source @ (TimelineHoverSourceIdentity::DirectMediaUrl(_)
            | TimelineHoverSourceIdentity::YouTubeUrl { .. }) => Some(active_source.clone()),
            TimelineHoverSourceIdentity::LocalFile(_) => None,
        }
    }

    /// Открывает independent source для active-playback hover executor-а.
    pub(crate) fn open_active_source(&self) -> TimelineHoverSourceOpenOutcome {
        match self.active_source.as_ref() {
            Some(TimelineHoverSourceIdentity::LocalFile(path)) => self.open_local_source(path),
            Some(TimelineHoverSourceIdentity::DirectMediaUrl(source_url)) => {
                self.open_direct_media_source(source_url)
            }
            Some(TimelineHoverSourceIdentity::YouTubeUrl {
                source_url,
                selected_stream_identity,
            }) => self.open_youtube_source(source_url, selected_stream_identity),
            None => TimelineHoverSourceOpenOutcome::MissingActiveSource,
        }
    }

    fn open_local_source(&self, path: &Path) -> TimelineHoverSourceOpenOutcome {
        match local_media::open_local_demuxer(path, &self.demux_config) {
            Ok(demuxer) => TimelineHoverSourceOpenOutcome::Opened(
                TimelineHoverOpenedSource::from_demuxer(demuxer),
            ),
            Err(_error) => TimelineHoverSourceOpenOutcome::OpenFailed {
                source_kind: TimelineHoverOpenFailedSourceKind::LocalFile,
            },
        }
    }

    fn open_direct_media_source(&self, source_url: &str) -> TimelineHoverSourceOpenOutcome {
        match service_direct_media::open_direct_media_url(
            source_url,
            &self.network_config,
            &self.demux_config,
        ) {
            Ok(opened_media) => TimelineHoverSourceOpenOutcome::Opened(
                TimelineHoverOpenedSource::from_demuxer(opened_media.into_demuxer()),
            ),
            Err(DirectMediaOpenError::NonSeekable { .. }) => {
                TimelineHoverSourceOpenOutcome::Unsupported {
                    source_kind: TimelineHoverUnsupportedSourceKind::DirectMediaUrl,
                }
            }
            Err(_error) => TimelineHoverSourceOpenOutcome::OpenFailed {
                source_kind: TimelineHoverOpenFailedSourceKind::DirectMediaUrl,
            },
        }
    }

    fn open_youtube_source(
        &self,
        source_url: &str,
        selected_stream_identity: &service_youtube::YoutubeSelectedStreamIdentity,
    ) -> TimelineHoverSourceOpenOutcome {
        match service_youtube::open_seekable_vod_from_selected_identity_with_demux_config(
            source_url,
            selected_stream_identity,
            &self.network_config,
            &self.youtube_config,
            &self.demux_config,
        ) {
            Ok(opened_media) => TimelineHoverSourceOpenOutcome::Opened(
                TimelineHoverOpenedSource::from_demuxer(opened_media.demuxer),
            ),
            Err(
                YoutubeSeekableVodOpenError::LiveStream
                | YoutubeSeekableVodOpenError::RangeUnsupported,
            ) => TimelineHoverSourceOpenOutcome::Unsupported {
                source_kind: TimelineHoverUnsupportedSourceKind::YouTubeUrl,
            },
            Err(YoutubeSeekableVodOpenError::Open(_error)) => {
                TimelineHoverSourceOpenOutcome::OpenFailed {
                    source_kind: TimelineHoverOpenFailedSourceKind::YouTubeUrl,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn audio_fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-assets/audio/music_sample.wav")
    }

    fn open_demuxer(factory: &TimelineHoverSourceFactory) -> Box<dyn Demuxer + Send> {
        match factory.open_active_source() {
            TimelineHoverSourceOpenOutcome::Opened(source) => source.into_demuxer(),
            other_outcome => panic!("expected opened local hover source, got {other_outcome:?}"),
        }
    }

    #[test]
    fn local_source_opens_independent_hover_demuxers() {
        let mut factory = TimelineHoverSourceFactory::new(PlayerDemuxConfig::default());
        factory.set_active_source(TimelineHoverSourceIdentity::LocalFile(audio_fixture_path()));

        let mut first_demuxer = open_demuxer(&factory);
        let mut second_demuxer = open_demuxer(&factory);

        let first_packet = first_demuxer
            .next_packet()
            .expect("first hover demuxer must read")
            .expect("fixture must contain a first packet");
        let second_packet = second_demuxer
            .next_packet()
            .expect("second hover demuxer must read")
            .expect("fixture must contain a first packet");

        assert_eq!(
            first_packet, second_packet,
            "каждый hover open должен начинаться с начала source, а не делить cursor"
        );
    }

    #[test]
    fn direct_network_source_returns_typed_open_failure_without_network_io_for_invalid_url() {
        let mut factory = TimelineHoverSourceFactory::new(PlayerDemuxConfig::default());

        factory.set_active_source(TimelineHoverSourceIdentity::DirectMediaUrl(
            "not-a-valid-url".to_string(),
        ));
        assert!(matches!(
            factory.open_active_source(),
            TimelineHoverSourceOpenOutcome::OpenFailed {
                source_kind: TimelineHoverOpenFailedSourceKind::DirectMediaUrl,
            }
        ));
    }

    #[test]
    fn media_switch_invalidates_old_hover_source_context() {
        let mut factory = TimelineHoverSourceFactory::new(PlayerDemuxConfig::default());
        factory.set_active_source(TimelineHoverSourceIdentity::LocalFile(audio_fixture_path()));
        assert!(matches!(
            factory.open_active_source(),
            TimelineHoverSourceOpenOutcome::Opened(_)
        ));

        factory.invalidate_active_source();

        assert!(matches!(
            factory.open_active_source(),
            TimelineHoverSourceOpenOutcome::MissingActiveSource
        ));
    }

    #[test]
    fn local_open_failure_returns_typed_failure_without_clearing_source_context() {
        let mut factory = TimelineHoverSourceFactory::new(PlayerDemuxConfig::default());
        factory.set_active_source(TimelineHoverSourceIdentity::LocalFile(PathBuf::from(
            "/tmp/rustiplayer-missing-hover-source.wav",
        )));

        assert!(matches!(
            factory.open_active_source(),
            TimelineHoverSourceOpenOutcome::OpenFailed {
                source_kind: TimelineHoverOpenFailedSourceKind::LocalFile,
            }
        ));
        assert!(matches!(
            factory.open_active_source(),
            TimelineHoverSourceOpenOutcome::OpenFailed {
                source_kind: TimelineHoverOpenFailedSourceKind::LocalFile,
            }
        ));
    }
}
