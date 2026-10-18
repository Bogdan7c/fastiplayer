//! Моменты появления слов внутри события (`TextSpan::reveal_at`).
//!
//! Реплика с постепенно появляющимися словами (WebVTT-караоке, YouTube srv3) меняет картинку
//! без смены набора событий, поэтому такие моменты тоже считаются изменениями шкалы.

use crate::event::{SubtitleContent, SubtitleEvent};
use crate::time::SubtitleTime;

/// Моменты появления слов события, лежащие строго внутри его показа `(start, end)`.
///
/// Момент не позже начала не создаёт изменения (слово видно сразу), момент не раньше конца —
/// тоже (слово так и не появится на экране). Для событий без текста (ASS, картинки) пусто:
/// у ASS караоке считает растеризатор по своим часам.
pub(super) fn reveal_times_within(event: &SubtitleEvent) -> impl Iterator<Item = SubtitleTime> {
    let (start, end) = (event.start(), event.end());
    let spans = match event.content() {
        SubtitleContent::Text(cue) => cue.text.spans(),
        _ => &[],
    };
    spans
        .iter()
        .filter_map(|span| span.reveal_at)
        .filter(move |&reveal_at| start < reveal_at && reveal_at < end)
}
