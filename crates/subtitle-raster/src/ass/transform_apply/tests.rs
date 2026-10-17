use subtitle_core::{AssFontSize, AssOverride, AssTransformTiming};

use super::*;
use crate::ass::test_script::{transform, transform_range};

/// Получатель, запоминающий (тег, вес).
#[derive(Default)]
struct Recorder {
    seen: Vec<(AssOverride, f64)>,
}

impl TagSink for Recorder {
    fn apply_tag(&mut self, tag: &AssOverride, weight: TagWeight) {
        self.seen.push((tag.clone(), weight.value()));
    }
}

fn font_size(value: f64) -> AssOverride {
    AssOverride::FontSize(AssFontSize::Absolute(value))
}

fn clock(elapsed_ms: i64, duration_ms: i64) -> AssEventClock {
    use subtitle_core::SubtitleTime;
    AssEventClock::new(
        SubtitleTime::from_millis(elapsed_ms),
        crate::renderer::AssEventTiming::new(
            SubtitleTime::ZERO,
            SubtitleTime::from_millis(duration_ms),
        ),
    )
}

fn weights(recorder: &Recorder) -> Vec<f64> {
    recorder.seen.iter().map(|(_, weight)| *weight).collect()
}

#[test]
fn nested_tags_get_the_weight_of_their_transform() {
    let AssOverride::Transform(outer) = transform_range(0, 1000, vec![font_size(50.0)]) else {
        unreachable!();
    };
    let mut recorder = Recorder::default();
    apply_transform(&mut recorder, &outer, Some(&clock(250, 4000)));
    assert_eq!(weights(&recorder), vec![0.25]);
}

#[test]
fn inner_transform_replaces_the_weight_for_itself_and_the_tags_after_it() {
    // Внешний \t идёт 0..2000, внутренний 0..1000. Вес внутреннего ЗАМЕНЯЕТ внешний (не
    // перемножается) и действует до конца списка, в том числе на тег после него.
    let inner = transform_range(0, 1000, vec![font_size(70.0)]);
    let AssOverride::Transform(outer) =
        transform_range(0, 2000, vec![font_size(50.0), inner, font_size(60.0)])
    else {
        unreachable!();
    };
    let mut at_500 = Recorder::default();
    apply_transform(&mut at_500, &outer, Some(&clock(500, 4000)));
    // Тег до внутреннего — вес внешнего (500/2000), внутренний и хвост — вес внутреннего (500/1000).
    assert_eq!(weights(&at_500), vec![0.25, 0.5, 0.5]);

    let mut at_1000 = Recorder::default();
    apply_transform(&mut at_1000, &outer, Some(&clock(1000, 4000)));
    // Внешний 1000/2000 = 0.5; внутренний завершился (1000 >= 1000) -> 1.
    assert_eq!(weights(&at_1000), vec![0.5, 1.0, 1.0]);
}

#[test]
fn invalid_transform_inside_transform_still_replaces_the_weight() {
    // Вложенный `\t` без вложенных тегов (некорректная форма) тоже подменяет вес: libass ставит
    // `pwr = k` до проверки формы. Его собственный k — по умолчанию (0..длительность).
    let empty_inner = transform(AssTransformTiming::Default, Vec::new());
    let AssOverride::Transform(outer) =
        transform_range(0, 4000, vec![font_size(50.0), empty_inner, font_size(60.0)])
    else {
        unreachable!();
    };
    let mut recorder = Recorder::default();
    apply_transform(&mut recorder, &outer, Some(&clock(1000, 2000)));
    // Внешний: 1000/4000 = 0.25. Внутренний пустой: 1000/2000 = 0.5 действует на хвост.
    assert_eq!(weights(&recorder), vec![0.25, 0.5]);
}

#[test]
fn without_a_clock_transforms_count_as_finished() {
    let AssOverride::Transform(outer) = transform_range(0, 1000, vec![font_size(50.0)]) else {
        unreachable!();
    };
    let mut recorder = Recorder::default();
    apply_transform(&mut recorder, &outer, None);
    assert_eq!(weights(&recorder), vec![1.0]);
}
