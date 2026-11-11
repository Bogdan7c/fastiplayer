//! Правила выбора часов: что сказать странице и про какое поколение (09a2, 16a2).

use super::*;

const TARGET: Duration = Duration::from_secs(12);

fn generation(number: u32) -> StreamGeneration {
    StreamGeneration::new(number).expect("generation")
}

/// Ждущее поколение 2, загруженное 1.
const PENDING: u32 = 2;
const LOADED: u32 = 1;

fn loaded(shown: LoadedMediaShown) -> LoadedClockWord {
    LoadedClockWord {
        generation: generation(LOADED),
        shown,
    }
}

fn clock(source: PageClockSource, number: u32) -> PageClock {
    PageClock {
        source,
        generation: generation(number),
    }
}

/// Порядок правил: цель главнее нового источника, новый источник главнее показанного.
#[test]
fn the_rules_apply_in_order() {
    use LoadedMediaShown::{SeekNotAnswered, Shown, StillInstalling};
    use NewSourceStance::{AwaitingDataWithoutTarget, NoneOrTargeted};
    let without_target = StillInstalling(GenerationStart::FirstCommonData);
    let at_target = StillInstalling(GenerationStart::PageSeekedTo(TARGET));
    let pending = |target, stance| PendingClockWord::of(generation(PENDING), target, stance);
    assert_eq!(
        PageClock::choose(
            pending(Some(TARGET), AwaitingDataWithoutTarget),
            loaded(Shown)
        )
        .source,
        PageClockSource::AwaitingDataAtTarget(TARGET)
    );
    assert_eq!(
        PageClock::choose(pending(None, AwaitingDataWithoutTarget), loaded(Shown)).source,
        PageClockSource::NewSourceAwaitingData
    );
    assert_eq!(
        PageClock::choose(pending(None, NoneOrTargeted), loaded(Shown)).source,
        PageClockSource::ShownGeneration
    );
    assert_eq!(
        PageClock::choose(PendingClockWord::Silent, loaded(without_target)).source,
        PageClockSource::InstallingGeneration
    );
    // Ставящееся поколение с целью: страница ждёт его там, а не в нуле.
    assert_eq!(
        PageClock::choose(PendingClockWord::Silent, loaded(at_target)).source,
        PageClockSource::AwaitingDataAtTarget(TARGET)
    );
    // Показанное поколение с перемоткой без ответа: страница уже на цели.
    assert_eq!(
        PageClock::choose(PendingClockWord::Silent, loaded(SeekNotAnswered(TARGET))).source,
        PageClockSource::AwaitingDataAtTarget(TARGET)
    );
}

/// Правила 1–2 описывают ЖДУЩЕЕ поколение: тик несёт его номер, хотя плеер ещё показывает
/// загруженное.
#[test]
fn the_pending_rules_name_the_pending_generation() {
    use NewSourceStance::{AwaitingDataWithoutTarget, NoneOrTargeted};
    let at_target = PendingClockWord::of(generation(PENDING), Some(TARGET), NoneOrTargeted);
    assert_eq!(
        PageClock::choose(at_target, loaded(LoadedMediaShown::Shown)),
        clock(PageClockSource::AwaitingDataAtTarget(TARGET), PENDING)
    );
    let without_target = PendingClockWord::of(generation(PENDING), None, AwaitingDataWithoutTarget);
    assert_eq!(
        PageClock::choose(without_target, loaded(LoadedMediaShown::Shown)),
        clock(PageClockSource::NewSourceAwaitingData, PENDING)
    );
}

/// Правила 3–5 описывают ЗАГРУЖЕННОЕ поколение — и «жду в цели» у него тоже его номер, а не
/// ждущего (тот же вариант источника рождается обеими дорогами).
#[test]
fn the_loaded_rules_name_the_loaded_generation_even_when_waiting_at_a_target() {
    let silent_pending =
        PendingClockWord::of(generation(PENDING), None, NewSourceStance::NoneOrTargeted);
    assert_eq!(silent_pending, PendingClockWord::Silent);
    let cases = [
        (LoadedMediaShown::Shown, PageClockSource::ShownGeneration),
        (
            LoadedMediaShown::StillInstalling(GenerationStart::FirstCommonData),
            PageClockSource::InstallingGeneration,
        ),
        (
            LoadedMediaShown::StillInstalling(GenerationStart::PageSeekedTo(TARGET)),
            PageClockSource::AwaitingDataAtTarget(TARGET),
        ),
        (
            LoadedMediaShown::SeekNotAnswered(TARGET),
            PageClockSource::AwaitingDataAtTarget(TARGET),
        ),
    ];
    for (shown, source) in cases {
        assert_eq!(
            PageClock::choose(silent_pending, loaded(shown)),
            clock(source, LOADED),
            "{shown:?}"
        );
    }
}
