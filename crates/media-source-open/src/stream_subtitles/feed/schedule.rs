//! Чистый планировщик ленты: какой сегмент скачивать следующим. Без сети и потоков.
//!
//! Правила:
//! - VOD и live с известным смещением «плейлист -> плеер»: окно `[позиция - behind,
//!   позиция + ahead]`; сначала сегменты от позиции ВПЕРЁД по возрастанию, затем те, что
//!   позади, от ближайшего к дальнему;
//! - live, пока смещение неизвестно: «хвост» плейлиста у живого края, самые новые сегменты
//!   первыми. Как только один сегмент даст точную привязку, окно считается по позиции.

use std::cmp::Reverse;
use std::time::Duration;

use crate::stream_subtitles::transport::{StreamSegment, StreamSegmentIndex, StreamSegmentKey};

/// Как переводить время плейлиста в шкалу плеера.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PlaylistOffset {
    /// Время плейлиста и плеера совпадают (VOD: шкала плеера строится по `EXTINF`).
    Identity,
    /// Выучено по сегменту с точной привязкой: `плеер = плейлист + смещение` (микросекунды).
    Learned(i64),
    /// Пока неизвестно (live до первой точной привязки).
    Unknown,
}

/// Входные данные одного решения планировщика.
#[derive(Clone, Copy, Debug)]
pub(super) struct WindowQuery<'a> {
    /// Текущий индекс сегментов.
    pub(super) index: &'a StreamSegmentIndex,
    /// Позиция поиска реплик на шкале плеера.
    pub(super) position: Duration,
    /// Окно вперёд.
    pub(super) ahead: Duration,
    /// Окно назад.
    pub(super) behind: Duration,
    /// Перевод времени плейлиста в шкалу плеера.
    pub(super) offset: PlaylistOffset,
}

/// Микросекунды с насыщением (длительности в конфиге могут быть любыми).
fn saturating_micros(duration: Duration) -> i64 {
    i64::try_from(duration.as_micros()).unwrap_or(i64::MAX)
}

/// Следующий сегмент для скачивания или `None`, если окно закрыто.
///
/// `is_excluded` - сегменты, которые трогать не нужно (уже загружены, пропущены или ждут).
pub(super) fn next_segment<'a>(
    query: &WindowQuery<'a>,
    is_excluded: impl Fn(StreamSegmentKey) -> bool,
) -> Option<&'a StreamSegment> {
    let candidates = query
        .index
        .segments
        .iter()
        .filter(|segment| !is_excluded(segment.key));
    match query.offset {
        PlaylistOffset::Identity => pick_around_position(query, 0, candidates),
        PlaylistOffset::Learned(offset) => pick_around_position(query, offset, candidates),
        PlaylistOffset::Unknown => pick_live_tail(query, candidates),
    }
}

/// Окно `[позиция - behind, позиция + ahead]` в координатах плейлиста (микросекунды).
struct PlaylistWindow {
    position: i64,
    start: i64,
    end: i64,
}

impl PlaylistWindow {
    fn new(query: &WindowQuery<'_>, offset_micros: i64) -> Self {
        let position = saturating_micros(query.position).saturating_sub(offset_micros);
        Self {
            position,
            start: position.saturating_sub(saturating_micros(query.behind)),
            end: position.saturating_add(saturating_micros(query.ahead)),
        }
    }

    /// Пересечение с окном: сегмент заканчивается после начала окна и начинается до его конца.
    fn intersects(&self, segment: &StreamSegment) -> bool {
        saturating_micros(segment.end()) > self.start && saturating_micros(segment.start) < self.end
    }
}

/// Лежит ли сегмент в окне ленты. `None` - окно пока не определено (live до первой точной
/// привязки): тогда судить нельзя, и сегмент надо считать нужным.
pub(super) fn is_in_window(query: &WindowQuery<'_>, segment: &StreamSegment) -> Option<bool> {
    let offset_micros = match query.offset {
        PlaylistOffset::Identity => 0,
        PlaylistOffset::Learned(offset) => offset,
        PlaylistOffset::Unknown => return None,
    };
    Some(PlaylistWindow::new(query, offset_micros).intersects(segment))
}

/// Окно вокруг позиции: вперёд по возрастанию, затем назад от ближайшего.
fn pick_around_position<'a>(
    query: &WindowQuery<'_>,
    offset_micros: i64,
    candidates: impl Iterator<Item = &'a StreamSegment>,
) -> Option<&'a StreamSegment> {
    let window = PlaylistWindow::new(query, offset_micros);
    let position_in_playlist = window.position;
    candidates
        .filter_map(|segment| {
            let start = saturating_micros(segment.start);
            let end = saturating_micros(segment.end());
            if !window.intersects(segment) {
                return None;
            }
            let rank = if end > position_in_playlist {
                (0_u8, start)
            } else {
                (1_u8, start.saturating_neg())
            };
            Some((rank, segment))
        })
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, segment)| segment)
}

/// Live без привязки: сегменты у живого края (суммарно `ahead + behind`), новые первыми.
fn pick_live_tail<'a>(
    query: &WindowQuery<'_>,
    candidates: impl Iterator<Item = &'a StreamSegment>,
) -> Option<&'a StreamSegment> {
    let playlist_end = query
        .index
        .segments
        .last()
        .map_or(0, |segment| saturating_micros(segment.end()));
    let tail_span = saturating_micros(query.ahead).saturating_add(saturating_micros(query.behind));
    let tail_start = playlist_end.saturating_sub(tail_span);
    candidates
        .filter(|segment| saturating_micros(segment.end()) > tail_start)
        .min_by_key(|segment| Reverse(saturating_micros(segment.start)))
}
