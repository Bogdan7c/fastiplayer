// Fastiplayer: выбор опорной cue-точки для seek.
//
// Вынесено из `demuxer.rs`, чтобы правило «в каких единицах сравнивать цель и CueTime» жило в
// одном месте и проверялось отдельно от EBML-итератора.

use crate::segment::{CuePointElement, SignedTrackTicks};

/// Выбирает cue-точку, с которой надо начинать чтение вперёд к цели `target`.
///
/// Правило:
/// - берётся последняя cue-точка, чьё время не позже цели (дальше reader дочитывает вперёд
///   до кадра, покрывающего цель);
/// - если цель раньше первой cue-точки (дорожка начинается позже нуля или цель отрицательна
///   из-за CodecDelay), берётся первая: данных раньше неё в файле нет, честное приземление —
///   первый кадр, и это лучше, чем отказ seek-а `OutOfRange`;
/// - `None` только для пустого списка cue-точек.
///
/// CueTime пересчитывается в ticks дорожки той же функцией, что и `Cluster/Timestamp` при
/// вычислении pts блока (`SegmentTicks::into_track_ticks`), поэтому сравнение идёт в одной шкале
/// с pts кадров, которые потом проверяет `seek_track_by_ts_forward`.
pub(crate) fn select_seek_cue_point(
    cue_points: &[CuePointElement],
    target: SignedTrackTicks,
    track_timestamp_scale: f64,
) -> Option<&CuePointElement> {
    // Последняя cue-точка не позже цели; cue-точки по спецификации идут по возрастанию времени.
    let last_cue_not_after_target = cue_points
        .iter()
        .take_while(|cue_point| cue_time_is_not_after(cue_point, target, track_timestamp_scale))
        .last();

    // Цель раньше всех cue-точек: начинаем с первой, а не отказываем в seek-е.
    last_cue_not_after_target.or_else(|| cue_points.first())
}

/// Сравнивает CueTime с целью в ticks дорожки.
fn cue_time_is_not_after(
    cue_point: &CuePointElement,
    target: SignedTrackTicks,
    track_timestamp_scale: f64,
) -> bool {
    // CueTime беззнаковый; если в i64 он не помещается, он заведомо позже любой цели.
    match cue_point.time.into_track_ticks(track_timestamp_scale).try_into_signed() {
        Some(cue_track_ticks) => cue_track_ticks <= target,
        None => false,
    }
}
