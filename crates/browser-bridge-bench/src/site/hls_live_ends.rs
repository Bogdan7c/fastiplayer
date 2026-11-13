//! Маршрут `/cdn/hls-live-ends/<прогон>/…` (сессия 16d1): «живой эфир» HLS, который КОНЧАЕТСЯ.
//!
//! Зачем: `hls-live` (10a3) никогда не пишет `EXT-X-ENDLIST`, поэтому он не умеет проверять момент,
//! когда эфир превращается в запись. Этот маршрут строит эфир так же (сегменты ролика `hls-ts-live`,
//! скользящее окно, часы от первого запроса плейлиста, см. [`super::hls_live`]), но после
//! [`ENDS_AFTER_PUBLISHED_SEGMENTS`] опубликованных сегментов:
//! - в плейлист дописывается `#EXT-X-ENDLIST`;
//! - окно ЗАМИРАЕТ (последние `WINDOW_SEGMENTS` сегментов, `MEDIA-SEQUENCE` больше не растёт);
//! - новые сегменты не публикуются.
//!
//! Браузер в `<video src>` на этом видит: `duration` была `Infinity`, после `ENDLIST` стала конечной
//! (одно `durationchange`), затем `ended`. Под тенью экстеншена то же обязана показать тень по
//! сообщению плеера `recording_window` (протокол 1.11).
//!
//! `hls-live` НЕ тронут: у него `ENDLIST` не пишется никогда, и его судья на это опирается. Здесь
//! переиспользуются только чистые части модели эфира (разбор исходного плейлиста, окно, доступ к
//! сегментам, часы прогонов); конец эфира — отдельное правило [`ending_window_at`].
//!
//! Если ролик короче [`ENDS_AFTER_PUBLISHED_SEGMENTS`], эфир кончается вместе с роликом (все
//! сегменты опубликованы): иначе окно замерло бы без `ENDLIST`, как у `hls-live`.
//!
//! Заголовки и правила 404 (токен без плейлиста, будущий сегмент, чужое имя) — как у `hls-live`.

use std::time::{Duration, Instant};

use super::hls_live::{
    LiveWindow, PLAYLIST_CONTENT_TYPE, PLAYLIST_NAME, SOURCE_DIRECTORY, SegmentAccess,
    SourcePlaylist, build_playlist, is_valid_token, live_headers, load_source, segment_access,
    window_at,
};
use super::http::{Body, Response};
use super::routes::RouteContext;
use super::static_files::serve_file;
use crate::media::safe_path::resolve_existing_file;

/// Префикс маршрута (под `/cdn/`, чтобы запросы попадали в журнал `request_log`).
pub const ROUTE_PREFIX: &str = "/cdn/hls-live-ends/";
/// Сколько сегментов опубликовано к моменту `ENDLIST`. При публикации раз в 2 с и четырёх сегментах
/// в начальном окне эфир кончается через (22 - 4) * 2 = 36 с после первого запроса плейлиста: робот
/// успевает передать вкладку и убедиться, что эфир ещё идёт (`duration === Infinity`).
pub const ENDS_AFTER_PUBLISHED_SEGMENTS: usize = 22;
/// Строка конца эфира в плейлисте.
const ENDLIST_LINE: &str = "#EXT-X-ENDLIST\n";

/// Состояние эфира в один момент: окно плейлиста и признак «эфир кончился».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EndingWindow {
    /// Какие сегменты опубликованы и какие видит плейлист.
    pub window: LiveWindow,
    /// Эфир кончился: плейлист заканчивается `ENDLIST`, окно больше не едет.
    pub ended: bool,
}

/// Окно эфира через `elapsed` после первого запроса плейлиста; эфир кончается, когда опубликовано
/// `ends_after` сегментов (или все сегменты ролика, если их меньше). Чистая функция.
pub(super) fn ending_window_at(
    elapsed: Duration,
    total_segments: usize,
    publish_period: Duration,
    ends_after: usize,
) -> EndingWindow {
    // Потолок публикации: дальше сегментов нет. Окно обычной модели упирается в него само
    // («замерзает»), а мы добавляем к этому ещё и признак конца.
    let ceiling = total_segments.min(ends_after);
    let window = window_at(elapsed, ceiling, publish_period);
    EndingWindow {
        window,
        ended: window.published >= ceiling,
    }
}

/// Плейлист для состояния эфира: как у `hls-live`, плюс `ENDLIST`, когда эфир кончился.
pub(super) fn build_ending_playlist(source: &SourcePlaylist, state: EndingWindow) -> String {
    let mut text = build_playlist(source, state.window);
    if state.ended {
        text.push_str(ENDLIST_LINE);
    }
    text
}

/// Ответ на запрос `relative` (путь после [`ROUTE_PREFIX`]: `<прогон>/stream.m3u8` или `<прогон>/seg-NNNNN.ts`).
pub(super) fn serve(context: &RouteContext, relative: &str, range: Option<&str>) -> Response {
    let Some((token, name)) = relative.split_once('/') else {
        return Response::text(404, "not found");
    };
    if !is_valid_token(token) || name.contains('/') {
        return Response::text(404, "not found");
    }
    let media_root = &context.shared.media.root;
    let Some(source) = load_source(media_root) else {
        return Response::text(
            500,
            "the source playlist of the live stream is not readable",
        );
    };
    let publish_period = Duration::from_secs(u64::from(source.target_duration_s));
    let now = Instant::now();
    let state_at = |elapsed: Duration| {
        ending_window_at(
            elapsed,
            source.segments.len(),
            publish_period,
            ENDS_AFTER_PUBLISHED_SEGMENTS,
        )
    };
    if name == PLAYLIST_NAME {
        let Some(elapsed) = context.shared.hls_live_ends.start_or_elapsed(token, now) else {
            return Response::text(503, "too many live runs remembered");
        };
        return live_headers(
            Response::new(200)
                .with_header("Content-Type", PLAYLIST_CONTENT_TYPE)
                .with_body(Body::Bytes(
                    build_ending_playlist(&source, state_at(elapsed)).into_bytes(),
                )),
        );
    }
    let Some(elapsed) = context.shared.hls_live_ends.elapsed_if_started(token, now) else {
        return Response::text(404, "no playlist was requested for this run");
    };
    match segment_access(&source, state_at(elapsed).window, name) {
        SegmentAccess::UnknownName => Response::text(404, "unknown segment"),
        SegmentAccess::NotYetPublished => Response::text(404, "segment is not published yet"),
        SegmentAccess::Published => {
            let relative_file = format!("{SOURCE_DIRECTORY}/{name}");
            match resolve_existing_file(media_root, &relative_file) {
                Some(path) => live_headers(serve_file(&path, range)),
                None => Response::text(404, "not found"),
            }
        }
    }
}

#[cfg(test)]
mod tests;
