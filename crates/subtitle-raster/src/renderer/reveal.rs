//! Поэтапное появление слов реплики простого формата (`TextSpan::reveal_at`).
//!
//! YouTube-подобное поведение: спан, чей момент появления ещё не наступил, НЕ рисуется и НЕ
//! занимает места в раскладке — строка растёт по мере появления слов. Владелец правила здесь:
//! растеризатор знает момент кадра ([`super::RenderRequest::moment`]) и то, что именно нарисовано.
//!
//! Три намерения:
//! - [`hidden_spans`] — какие спаны скрыты на момент (входит в ключ кэша реплики: разные наборы
//!   скрытых спанов — разные картинки);
//! - [`next_reveal_after`] — когда набор скрытых спанов изменится (срок годности кадра);
//! - [`cue_without_spans`] — реплика без скрытых спанов (её и рисуем).

use std::borrow::Cow;

use subtitle_core::{StyledText, SubtitleTextCue, SubtitleTime};

/// Индексы (в порядке спанов) спанов, ещё не появившихся на `moment`.
///
/// Пустой срез (без выделения памяти) означает «реплика видна целиком»: такая реплика делит
/// запись кэша с репликой без `reveal_at` вообще.
pub(super) fn hidden_spans(cue: &SubtitleTextCue, moment: SubtitleTime) -> Box<[u32]> {
    cue.text
        .spans()
        .iter()
        .enumerate()
        .filter(|(_, span)| span.reveal_at.is_some_and(|reveal_at| reveal_at > moment))
        .filter_map(|(index, _)| u32::try_from(index).ok())
        .collect()
}

/// Реплика содержит хотя бы один спан с отложенным появлением.
pub(super) fn has_reveal_times(cue: &SubtitleTextCue) -> bool {
    cue.text.spans().iter().any(|span| span.reveal_at.is_some())
}

/// Ближайший момент строго после `moment`, когда появится очередной спан; `None` — больше
/// ничего не появится (все спаны уже видны).
pub(super) fn next_reveal_after(
    cue: &SubtitleTextCue,
    moment: SubtitleTime,
) -> Option<SubtitleTime> {
    cue.text
        .spans()
        .iter()
        .filter_map(|span| span.reveal_at)
        .filter(|&reveal_at| reveal_at > moment)
        .min()
}

/// Реплика без скрытых спанов. Если скрывать нечего, возвращается исходная (без копии).
pub(super) fn cue_without_spans<'cue>(
    cue: &'cue SubtitleTextCue,
    hidden: &[u32],
) -> Cow<'cue, SubtitleTextCue> {
    if hidden.is_empty() {
        return Cow::Borrowed(cue);
    }
    let visible_spans = cue
        .text
        .spans()
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            // `hidden` отсортирован по построению (`hidden_spans`), но порядок здесь не важен.
            u32::try_from(*index).is_ok_and(|index| !hidden.contains(&index))
        })
        .map(|(_, span)| span.clone())
        .collect();
    Cow::Owned(SubtitleTextCue {
        text: StyledText::new(visible_spans),
        placement: cue.placement.clone(),
    })
}

#[cfg(test)]
mod tests {
    use subtitle_core::{CuePlacement, SpanStyle, TextSpan};

    use super::*;

    fn cue_with_reveals(words: &[(&str, Option<i64>)]) -> SubtitleTextCue {
        let spans = words
            .iter()
            .map(|(text, reveal_millis)| {
                let mut span = TextSpan::text(*text, SpanStyle::default());
                span.reveal_at = reveal_millis.map(SubtitleTime::from_millis);
                span
            })
            .collect();
        SubtitleTextCue {
            text: StyledText::new(spans),
            placement: CuePlacement::default(),
        }
    }

    #[test]
    fn hides_only_spans_revealed_strictly_after_moment() {
        let cue = cue_with_reveals(&[("a", None), (" b", Some(100)), (" c", Some(200))]);
        let at = |millis| hidden_spans(&cue, SubtitleTime::from_millis(millis));
        assert_eq!(&*at(99), &[1, 2]);
        assert_eq!(
            &*at(100),
            &[2],
            "a span is visible exactly at its reveal time"
        );
        assert_eq!(&*at(200), &[] as &[u32]);
    }

    #[test]
    fn next_reveal_is_strictly_after_moment_and_none_when_all_visible() {
        let cue = cue_with_reveals(&[("a", None), (" b", Some(100)), (" c", Some(200))]);
        let next = |millis| next_reveal_after(&cue, SubtitleTime::from_millis(millis));
        assert_eq!(next(0), Some(SubtitleTime::from_millis(100)));
        assert_eq!(next(100), Some(SubtitleTime::from_millis(200)));
        assert_eq!(next(200), None);
    }

    #[test]
    fn filtered_cue_drops_hidden_spans_and_borrows_when_nothing_is_hidden() {
        let cue = cue_with_reveals(&[("a", None), (" b", Some(100))]);
        let filtered = cue_without_spans(&cue, &[1]);
        assert_eq!(filtered.text.plain_text(), "a");
        assert!(matches!(cue_without_spans(&cue, &[]), Cow::Borrowed(_)));
    }
}
