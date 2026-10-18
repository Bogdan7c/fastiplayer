//! Лента субтитров потоковых источников (HLS, затем DASH): нейтральная модель, контракт
//! транспорта, HLS-реализация и фоновая лента с политикой окон, сбоев и ожидания.
//!
//! Слои:
//! - [`StreamSubtitleSource`] / [`StreamSubtitleTrackInfo`] - безопасное описание дорожек;
//! - [`StreamSubtitleTransport`] - сеть и формат сегмента одного протокола (реплики сразу
//!   на шкале плеера); HLS - [`HlsStreamSubtitleTransport`];
//! - [`StreamSubtitleFeed`] - поток, который по позиции воспроизведения решает, что качать.
//!
//! Инварианты: видео не ждёт субтитры; секреты (URL/query) не попадают в `Debug`, логи и
//! сообщения; закрытие ленты не блокирует вызывающий поток.

mod dash;
mod error;
mod feed;
mod hls;
mod model;
mod transport;

#[cfg(test)]
mod tests;

pub use dash::DashStreamSubtitleTransport;
pub use error::{StreamSubtitleError, StreamSubtitleFailureKind, StreamSubtitleUnsupported};
pub use feed::{
    StreamPlaybackPosition, StreamSegmentSkipReason, StreamSubtitleEvents, StreamSubtitleFeed,
    StreamSubtitleFeedConfig, StreamSubtitleFeedStartError, StreamSubtitleMessage,
    StreamTrackUnavailableReason,
};
pub use hls::HlsStreamSubtitleTransport;
pub use model::{StreamSubtitleSource, StreamSubtitleTrackId, StreamSubtitleTrackInfo};
pub use transport::{
    DecodedSegment, PendingSegmentDecode, PlayerTimeSpan, SegmentDecode, StreamIndexRefresh,
    StreamSegment, StreamSegmentIndex, StreamSegmentKey, StreamSegmentLimits, StreamSegmentRef,
    StreamSubtitleTransport, StreamTimelineEpoch,
};
