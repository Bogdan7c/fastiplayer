//! Маршрут `/cdn/hls-live-sleep/<прогон>/…` (сессия 16d4): «живой эфир» HLS, сервер которого на время
//! «засыпает».
//!
//! Зачем: в 16d3 движок HLS-эфира научился двум вещам — ждать сеть (`network.reconnect_wait_ms`) и
//! склеивать окно, в котором не осталось ни одного прочитанного сегмента. Доказано это было только
//! тестами с поддельной страницей. Этот маршрут проверяет оба механизма разом с НАСТОЯЩИМ
//! экстеншеном: так выглядит эфир для машины, которая проспала, пока эфир шёл дальше.
//!
//! Модель (чистая функция [`sleeping_state_at`], часы — от первого запроса плейлиста, как у
//! [`super::hls_live`]):
//! - до [`SLEEP_SCHEDULE`]`.falls_asleep_after` эфир обычный: скользящее окно сегментов `hls-ts-live`;
//! - потом [`SLEEP_SCHEDULE`]`.sleeps_for` плейлист отвечает 503, а окно на сервере всё это время едет
//!   дальше по тем же часам — сервер «спит» только для зрителя, не для эфира;
//! - после «пробуждения» плейлист снова 200, но окно уехало дальше, чем его длина: в нём нет ни одного
//!   сегмента, который был в плейлисте до сна (проверяет тест). Плееру остаётся только склейка.
//!
//! Сегменты 503 не получают: как настоящий CDN, сервер отдаёт уже опубликованный сегмент и во время
//! «сна» (план сессии: «засыпает» именно плейлист). Правила 404 и заголовки — как у `hls-live`.
//! `hls-live` и `hls-live-ends` не тронуты: здесь переиспользуются только чистые части модели эфира.

use std::time::{Duration, Instant};

use super::hls_live::{
    INITIAL_WINDOW_SEGMENTS, LiveWindow, PLAYLIST_CONTENT_TYPE, PLAYLIST_NAME, SOURCE_DIRECTORY,
    SegmentAccess, build_playlist, is_valid_token, live_headers, load_source, segment_access,
    window_at,
};
use super::http::{Body, Response};
use super::routes::RouteContext;
use super::static_files::serve_file;
use crate::media::safe_path::resolve_existing_file;

/// Префикс маршрута (под `/cdn/`, чтобы запросы и их статусы попадали в журнал `request_log`).
pub const ROUTE_PREFIX: &str = "/cdn/hls-live-sleep/";
/// Статус плейлиста, пока сервер «спит»: 503 Service Unavailable — временная недоступность.
pub const ASLEEP_STATUS: u16 = 503;

/// Когда и надолго ли сервер эфира «засыпает» (отсчёт — от первого запроса плейлиста прогона).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SleepSchedule {
    /// Сколько эфир идёт нормально до «сна».
    pub falls_asleep_after: Duration,
    /// Сколько длится «сон» (плейлист — 503).
    pub sleeps_for: Duration,
}

impl SleepSchedule {
    /// Момент «пробуждения» от первого запроса плейлиста.
    pub const fn wakes_at(self) -> Duration {
        self.falls_asleep_after.saturating_add(self.sleeps_for)
    }
}

/// Расписание сна маршрута.
///
/// - 30 с бодрствования: странице нужно до ~8 с, чтобы заиграть, роботу — передать вкладку, плееру —
///   показать первые кадры. Судья строки проверяет, что всё это случилось ДО сна, иначе прогон негоден.
/// - 20 с сна: за это время окно уезжает на 10 сегментов при длине окна 4 — общего сегмента нет; и это
///   втрое меньше бюджета ожидания сети плеера (60 с), значит, плеер обязан дождаться.
pub const SLEEP_SCHEDULE: SleepSchedule = SleepSchedule {
    falls_asleep_after: Duration::from_secs(30),
    sleeps_for: Duration::from_secs(20),
};

/// Период публикации сегментов ролика `hls-ts-live` (его `TARGETDURATION`, 2 с). Нужен вне маршрута
/// (судье), где исходного плейлиста нет; маршрут сверяет его с настоящим плейлистом ролика в тесте.
pub const SOURCE_SEGMENT_PERIOD: Duration = Duration::from_secs(2);

/// Номер первого сегмента, которого НЕ было ни в одном плейлисте до сна: сколько сегментов опубликовано
/// к моменту засыпания. Всё, что фон качает с таким номером и дальше, — сегменты нового окна.
///
/// Это `published` окна [`window_at`] в момент засыпания (тест сверяет). Порог с запасом: в самый
/// момент засыпания плейлист уже 503, поэтому последний показанный до сна сегмент ещё на один-два
/// номера раньше.
pub fn first_segment_unseen_before_sleep(schedule: SleepSchedule, period: Duration) -> usize {
    // Нулевой период невозможен (константа), но защищаемся от деления на ноль, как `window_at`.
    let period_ms = period.as_millis().max(1);
    let grown =
        usize::try_from(schedule.falls_asleep_after.as_millis() / period_ms).unwrap_or(usize::MAX);
    INITIAL_WINDOW_SEGMENTS.saturating_add(grown)
}

/// Что сервер отвечает на плейлист в данный момент.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SleepingState {
    /// Сервер бодрствует: плейлист показывает это окно.
    Awake(LiveWindow),
    /// Сервер «спит»: плейлист отвечает [`ASLEEP_STATUS`]. Окно при этом продолжает ехать — оно здесь,
    /// чтобы сегменты отдавались по тем же часам.
    Asleep(LiveWindow),
}

impl SleepingState {
    /// Окно эфира по часам — и во сне, и наяву (по нему решается доступ к сегментам).
    pub(super) const fn window(self) -> LiveWindow {
        match self {
            Self::Awake(window) | Self::Asleep(window) => window,
        }
    }
}

/// Состояние эфира через `elapsed` после первого запроса плейлиста. Чистая функция.
///
/// Окно всегда считается обычной моделью эфира ([`window_at`]); расписание только решает, «спит» ли
/// сервер в этот момент. Сон — полуоткрытый интервал `[засыпание, пробуждение)`.
pub(super) fn sleeping_state_at(
    elapsed: Duration,
    total_segments: usize,
    publish_period: Duration,
    schedule: SleepSchedule,
) -> SleepingState {
    let window = window_at(elapsed, total_segments, publish_period);
    let asleep = elapsed >= schedule.falls_asleep_after && elapsed < schedule.wakes_at();
    if asleep {
        SleepingState::Asleep(window)
    } else {
        SleepingState::Awake(window)
    }
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
        sleeping_state_at(
            elapsed,
            source.segments.len(),
            publish_period,
            SLEEP_SCHEDULE,
        )
    };
    if name == PLAYLIST_NAME {
        let Some(elapsed) = context.shared.hls_live_sleep.start_or_elapsed(token, now) else {
            return Response::text(503, "too many live runs remembered");
        };
        return match state_at(elapsed) {
            SleepingState::Asleep(_) => {
                live_headers(Response::text(ASLEEP_STATUS, "the live origin is asleep"))
            }
            SleepingState::Awake(window) => live_headers(
                Response::new(200)
                    .with_header("Content-Type", PLAYLIST_CONTENT_TYPE)
                    .with_body(Body::Bytes(build_playlist(&source, window).into_bytes())),
            ),
        };
    }
    let Some(elapsed) = context.shared.hls_live_sleep.elapsed_if_started(token, now) else {
        return Response::text(404, "no playlist was requested for this run");
    };
    match segment_access(&source, state_at(elapsed).window(), name) {
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
