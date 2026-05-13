use std::sync::Arc;

use anyhow::{Context, Result};
use reqwest::StatusCode;
use source_core::{
    ByteSource, CancellationToken, HttpRangeSource, HttpRangeSourceConfig, Seekability,
    SourceError, SourceFingerprint, SourceResult, SourceRuntimeConfig, SourceValidators,
};

use crate::dto::{YoutubeDirectStreamDescriptor, YoutubeStreamKind};
use crate::resolver::YoutubeDirectStreamResolver;

/// Контекст refresh-а direct URL, ограниченный одним stream-ом.
#[derive(Clone)]
pub(crate) struct RefreshContext {
    /// Исходный URL страницы/ролика, который понимает yt-dlp.
    pub(crate) original_video_url: String,

    /// Какой stream нужно достать из свежей пары descriptors.
    pub(crate) stream_kind: YoutubeStreamKind,

    /// Resolver, который умеет заново получить direct URLs.
    pub(crate) resolver: Arc<dyn YoutubeDirectStreamResolver>,
}

/// HTTP Range source с одноразовым refresh-ом direct URL через service layer.
pub(crate) struct YoutubeRefreshingRangeSource {
    /// Текущий direct stream descriptor.
    descriptor: YoutubeDirectStreamDescriptor,

    /// Настройки HTTP/cache layer из пользовательского config.
    source_config: SourceRuntimeConfig,

    /// Активный neutral HTTP Range source.
    inner: HttpRangeSource,

    /// Контекст, через который можно один раз получить свежий direct URL.
    refresh_context: RefreshContext,

    /// Гарантия bounded retry: refresh выполняется максимум один раз.
    refresh_attempted: bool,
}

impl YoutubeRefreshingRangeSource {
    /// Открывает Range source; если initial direct URL уже истёк, refresh выполняется один раз.
    pub(crate) fn open(
        descriptor: YoutubeDirectStreamDescriptor,
        source_config: SourceRuntimeConfig,
        refresh_context: RefreshContext,
    ) -> Result<Self> {
        match open_http_range_source(&descriptor, source_config.clone()) {
            Ok(inner) => Ok(Self {
                descriptor,
                source_config,
                inner,
                refresh_context,
                refresh_attempted: false,
            }),
            Err(error) if direct_url_may_be_expired(&error) => {
                let refreshed_descriptor = refresh_descriptor(&refresh_context)
                    .context("Не удалось обновить истёкший YouTube direct URL")?;
                let inner = open_http_range_source(&refreshed_descriptor, source_config.clone())
                    .context("Не удалось открыть обновлённый YouTube HTTP Range source")?;
                Ok(Self {
                    descriptor: refreshed_descriptor,
                    source_config,
                    inner,
                    refresh_context,
                    refresh_attempted: true,
                })
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Возвращает текущий descriptor после возможного initial refresh-а.
    pub(crate) fn descriptor(&self) -> &YoutubeDirectStreamDescriptor {
        &self.descriptor
    }

    /// Повторяет текущую read-позицию после успешного refresh-а direct URL.
    fn refresh_once_at_position(&mut self, position: u64) -> bool {
        if self.refresh_attempted {
            return false;
        }

        self.refresh_attempted = true;
        let refreshed_descriptor = match refresh_descriptor(&self.refresh_context) {
            Ok(descriptor) => descriptor,
            Err(error) => {
                tracing::warn!(error = %error, "YouTube direct URL refresh failed");
                return false;
            }
        };
        let mut refreshed_source =
            match open_http_range_source(&refreshed_descriptor, self.source_config.clone()) {
                Ok(source) => source,
                Err(error) => {
                    tracing::warn!(error = %error, "Updated YouTube HTTP Range source open failed");
                    return false;
                }
            };

        if let Err(error) = refreshed_source.seek(position) {
            tracing::warn!(error = %error, position, "Updated YouTube source seek failed");
            return false;
        }

        self.descriptor = refreshed_descriptor;
        self.inner = refreshed_source;
        true
    }
}

impl ByteSource for YoutubeRefreshingRangeSource {
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        let retry_position = self.inner.position();
        match self.inner.read(output, cancellation) {
            Ok(bytes_read) => Ok(bytes_read),
            Err(error) if direct_url_may_be_expired(&error) => {
                if self.refresh_once_at_position(retry_position) {
                    self.inner.read(output, cancellation)
                } else {
                    Err(error)
                }
            }
            Err(error) => Err(error),
        }
    }

    fn seek(&mut self, offset: u64) -> SourceResult<()> {
        self.inner.seek(offset)
    }

    fn position(&self) -> u64 {
        self.inner.position()
    }

    fn seekability(&self) -> Seekability {
        if self.descriptor.live {
            return Seekability::NotSeekable {
                reason: source_core::NotSeekableReason::Unknown,
            };
        }

        self.inner.seekability()
    }

    fn validators(&self) -> SourceValidators {
        self.inner.validators()
    }

    fn content_length(&self) -> Option<u64> {
        self.inner.content_length()
    }

    fn fingerprint(&self) -> SourceFingerprint {
        self.inner.fingerprint()
    }
}

/// Открывает нейтральный HTTP Range source из service descriptor-а.
fn open_http_range_source(
    descriptor: &YoutubeDirectStreamDescriptor,
    source_config: SourceRuntimeConfig,
) -> SourceResult<HttpRangeSource> {
    HttpRangeSource::open(HttpRangeSourceConfig::new(
        descriptor.url.clone(),
        descriptor.headers.clone(),
        source_config,
    ))
}

/// Возвращает свежий descriptor нужного stream kind-а.
fn refresh_descriptor(refresh_context: &RefreshContext) -> Result<YoutubeDirectStreamDescriptor> {
    let refreshed_streams = refresh_context
        .resolver
        .resolve_direct_streams(&refresh_context.original_video_url)?;

    Ok(match refresh_context.stream_kind {
        YoutubeStreamKind::Video => refreshed_streams.video,
        YoutubeStreamKind::Audio => refreshed_streams.audio,
    })
}

/// Определяет HTTP failure, похожий на истёкший direct URL.
fn direct_url_may_be_expired(error: &SourceError) -> bool {
    match error {
        SourceError::HttpStatus { status, .. } => matches!(
            *status,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
        ),
        SourceError::HttpRequest { .. } => true,
        _ => false,
    }
}
