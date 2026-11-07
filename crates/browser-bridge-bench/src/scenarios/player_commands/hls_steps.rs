//! Шаги `player-commands` для родного HLS VOD (сессия 10a2): страница `hls-vod-long`.
//!
//! Пауза, play, перемотка в уже скачанное и возврат идут общим порядком (см. родительский модуль).
//! Здесь только то, что свойственно HLS: перемотка вперёд ЗА сегменты, которые фон ещё не скачал.
//! У HLS нет байтового «края окна» файла и нет `buffered` MSE-страницы, поэтому цель выбираем по
//! журналу сервера: берём самый дальний сегмент, которого фон до сих пор не запрашивал, и
//! требуем, чтобы после перемотки фон запросил новый сегмент у цели.

use std::time::Instant;

use super::super::capture::hls_evidence::{
    HlsPage, commands_hls_evidence, fetched_segment_indexes, seek_fetch_check,
    unfetched_seek_target,
};
use super::super::handoff_run::{ACK_TIMEOUT, HandoffRun};
use super::{
    MICROS_PER_SECOND, parse_buffer_report, pause_and_take_frame_baseline, tell_and_wait,
    wait_frames_beyond, wait_position_advance,
};
use crate::observe::FrameGrowth;
use crate::player_control::PlayerControl;
use crate::site::StandSite;

/// Сколько секунд ролика должно остаться после цели: сводки плеера приходят раз в 10 с, и после
/// перемотки нужно успеть получить следующую, пока ролик не кончился.
const ROOM_AFTER_SEEK_S: f64 = 12.0;

/// HLS-страница `player-commands`: где смотреть журнал сервера и с какого мига качает фон.
#[derive(Clone, Copy)]
pub(super) struct HlsContext<'site> {
    pub(super) page: HlsPage,
    pub(super) site: &'site StandSite,
    pub(super) handed_off_at: Instant,
}

impl<'site> HlsContext<'site> {
    pub(super) fn new(page: HlsPage, site: &'site StandSite, handed_off_at: Instant) -> Self {
        Self {
            page,
            site,
            handed_off_at,
        }
    }
}

/// Перемотка вперёд за сегменты, которые фон ещё не скачал. Доказательства: новый сегмент у цели
/// в журнале сервера (запрос фона), часы страницы у цели, кадры плеера растут после перемотки.
/// Возвращает улики числами.
pub(super) fn seek_beyond_fetched_segments(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    hls: HlsContext<'_>,
    seek_growth: &mut Result<FrameGrowth, String>,
) -> Result<String, String> {
    // На паузе плеер перестаёт тянуть сегменты впрок: «скачано» не уплывает, пока выбираем цель.
    let baseline = pause_and_take_frame_baseline(run, control)?;
    tell_and_wait(run, "report-buffer")?;
    let report = run.wait_page_log_line("buffer-report", ACK_TIMEOUT)?;
    let (_, duration_s) = parse_buffer_report(&report)
        .ok_or_else(|| format!("cannot read the page buffer report: {report:?}"))?;
    let requests = hls.site.media_requests();
    let fetched_before =
        fetched_segment_indexes(hls.page, &requests, hls.handed_off_at, Instant::now());
    let target_s = unfetched_seek_target(&fetched_before, duration_s, ROOM_AFTER_SEEK_S)
        .ok_or_else(|| {
            format!(
                "the background has already fetched every segment we could seek past (fetched: {fetched_before:?}, duration {duration_s} s): nothing is left to prove a network seek"
            )
        })?;
    let position_before = control
        .position_micros()
        .map_err(|error| format!("Position: {error}"))?;
    let target_micros = (target_s * MICROS_PER_SECOND) as i64;
    let seek_at = Instant::now();
    control
        .seek_by(target_micros - position_before)
        .map_err(|error| format!("Seek: {error}"))?;
    control.play().map_err(|error| format!("Play: {error}"))?;
    let advanced = wait_position_advance(control, target_micros);
    let page_agrees = tell_and_wait(run, &format!("expect-seek:{target_s:.2}"));
    *seek_growth = wait_frames_beyond(run, baseline);
    let fetched = seek_fetch_check(
        hls.page,
        &hls.site.media_requests(),
        &fetched_before,
        seek_at,
        target_s,
    );
    advanced?;
    page_agrees?;
    let facts = fetched?;
    seek_growth.clone().map(|_| facts)
}

/// Улики сервера для вердикта: провалы и сводка (вместе с итогом шага перемотки, если он шёл).
/// `returned_at` — когда закрыли окно плеера (`None` — до возврата не дошли).
pub(super) fn server_evidence(
    hls: HlsContext<'_>,
    returned_at: Option<Instant>,
    seek_facts: Option<&str>,
) -> (Vec<String>, String) {
    let (failures, summary) = commands_hls_evidence(
        hls.page,
        &hls.site.media_requests(),
        hls.handed_off_at,
        returned_at,
    );
    let seek = seek_facts.unwrap_or("the seek beyond the fetched segments did not run");
    (failures, format!("{summary}; {seek}"))
}
