//! С какого места начинается поколение передачи (живая проверка 09b2, запись Twitch):
//! плеер и страница подделаны, поток поколения — настоящий `pushed-media-source`.
//!
//! Страница стояла на паузе поздно в ролике, а начало уже стёрла. Журнал повторяет её
//! историю целиком, вместе со стёртым, поэтому посреди повтора полка держит и то, и другое.
//! Корень зависания: поколение уходило плееру раньше, чем он узнавал T0, и вставало на
//! начало истории. Перемотка снимала стёртое и просила страницу вернуться к нему: страница
//! бросала своё место и перекачивала начало, а плеер не показывал ни кадра.

use pushed_media_source::{PageRemovedRange, PageRemovedRangeEnd};

use super::generation_placement::{
    HOLD_CHECK, LONG_CLIP, LONG_WAIT, append_media_at, generation_with_head, page_seek_targets,
    page_time, player_seeks, start_of_load, with_wait,
};
use super::*;

/// Где стоит страница и лежат её байты: поздняя часть ролика.
const PAGE_POSITION: Duration = Duration::from_secs(120);
/// Другая цель страницы (дальше допуска совпадения перемоток).
const ELSEWHERE: Duration = Duration::from_secs(300);
/// Что крутить, чтобы цикл успел всё, что мог: отложенное срабатывает с первого прохода.
const SETTLE: Duration = Duration::from_millis(100);

/// Страница ещё до передачи стёрла начало (0–0,4 с): полка держит его только ради плеера.
fn erase_the_head(writer: &mut PushedStreamWriter) {
    for lane in [LaneId::new(0), LaneId::new(1)] {
        writer
            .note_page_removed(
                lane,
                PageRemovedRange::new(
                    Duration::ZERO,
                    PageRemovedRangeEnd::At(Duration::from_secs(1)),
                ),
            )
            .expect("the removal is accepted");
    }
}

/// Передача: страница открыла поколение; его поток и подделки — у теста.
fn opening_generation(duration: PushedDuration) -> (Harness, SessionRef, PushedStreamWriter) {
    let mut harness = with_wait(Harness::new(), LONG_WAIT);
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (writer, notice) = generation_with_head(&current, 1, duration);
    harness.media.on_intake(vec![notice], None);
    (harness, current, writer)
}

/// Как переходник: страница назвала позицию, источник узнаёт о ней в порядке провода.
fn page_names_position(
    harness: &mut Harness,
    current: &SessionRef,
    writer: &mut PushedStreamWriter,
    position: Duration,
) {
    writer.note_page_seek(position);
    harness.media.on_intake(
        vec![intent(
            current,
            PageIntent::FollowPageSeek(page_time(position)),
        )],
        None,
    );
}

/// Главное: T0 приходит раньше истории, поэтому начало истории плеер даже не видит: поколение
/// держится до данных у T0, встаёт на T0, а страницу никуда не зовут. Итоговое T0 (шаг 3
/// договора), пришедшее во время установки, — не новая перемотка.
#[test]
fn a_handoff_starts_at_the_position_the_page_named_before_its_history() {
    let (mut harness, current, mut writer) = opening_generation(PushedDuration::Finite(LONG_CLIP));
    page_names_position(&mut harness, &current, &mut writer, PAGE_POSITION);
    erase_the_head(&mut writer);
    harness.drive_for(HOLD_CHECK);
    assert_eq!(
        harness.load_count(),
        0,
        "the history alone must not start the generation"
    );

    append_media_at(&mut writer, PAGE_POSITION);
    harness.drive_until_loaded();
    assert_eq!(start_of_load(&harness, 0), Some(PAGE_POSITION));

    // Итоговое T0: плеер ещё ставит поколение.
    page_names_position(&mut harness, &current, &mut writer, PAGE_POSITION);
    harness.show_last_load(PAGE_POSITION);
    let mut after_show = harness.player.take_calls();
    after_show.extend(harness.drive_for(SETTLE));
    assert_eq!(
        player_seeks(&after_show),
        Vec::<Duration>::new(),
        "the generation already stands at T0: a repeated seek would drop the shown frame"
    );
    assert_eq!(
        page_seek_targets(&harness.page.take_sent()),
        Vec::<Duration>::new(),
        "the page is never dragged away from its own position"
    );
}

/// Страница не назвала позицию, а стёрла начало: поколение начинается там, где её данные, и
/// странице не шлют просьбу вернуться к стёртому.
#[test]
fn a_generation_without_a_position_starts_where_the_page_still_holds_data() {
    let (mut harness, _current, mut writer) = opening_generation(PushedDuration::Finite(LONG_CLIP));
    erase_the_head(&mut writer);
    append_media_at(&mut writer, PAGE_POSITION);
    harness.drive_until_loaded();

    assert_eq!(start_of_load(&harness, 0), Some(PAGE_POSITION));
    harness.drive_for(SETTLE);
    assert_eq!(
        page_seek_targets(&harness.page.take_sent()),
        Vec::<Duration>::new(),
        "the page is not asked to go back to what it erased"
    );
}

/// Поколение уже поставлено на T0, а страница успела уйти и вернуться: последняя перемотка
/// побеждает, и раз она совпала с местом установки, отложенной перемотки больше нет.
#[test]
fn returning_to_the_start_place_cancels_the_seek_deferred_until_shown() {
    let (mut harness, current, mut writer) = opening_generation(PushedDuration::Finite(LONG_CLIP));
    page_names_position(&mut harness, &current, &mut writer, PAGE_POSITION);
    append_media_at(&mut writer, PAGE_POSITION);
    harness.drive_until_loaded();

    page_names_position(&mut harness, &current, &mut writer, ELSEWHERE);
    page_names_position(&mut harness, &current, &mut writer, PAGE_POSITION);
    harness.show_last_load(PAGE_POSITION);
    let mut after_show = harness.player.take_calls();
    after_show.extend(harness.drive_for(SETTLE));
    assert_eq!(player_seeks(&after_show), Vec::<Duration>::new());
}

/// Другое место во время установки — настоящая перемотка: она ждёт показа.
#[test]
fn another_place_while_the_generation_installs_is_a_real_seek() {
    let (mut harness, current, mut writer) = opening_generation(PushedDuration::Finite(LONG_CLIP));
    page_names_position(&mut harness, &current, &mut writer, PAGE_POSITION);
    append_media_at(&mut writer, PAGE_POSITION);
    harness.drive_until_loaded();

    page_names_position(&mut harness, &current, &mut writer, ELSEWHERE);
    harness.show_last_load(PAGE_POSITION);
    let mut after_show = harness.player.take_calls();
    after_show.extend(harness.drive_for(SETTLE));
    assert_eq!(player_seeks(&after_show), vec![ELSEWHERE]);
}

/// После показа та же цель — настоящая перемотка страницы назад: позиция уже ушла.
#[test]
fn the_start_place_after_the_show_is_a_real_seek() {
    let (mut harness, current, mut writer) = opening_generation(PushedDuration::Finite(LONG_CLIP));
    page_names_position(&mut harness, &current, &mut writer, PAGE_POSITION);
    append_media_at(&mut writer, PAGE_POSITION);
    harness.drive_until_loaded();
    harness.show_last_load(PAGE_POSITION);
    harness.player.take_calls();

    page_names_position(&mut harness, &current, &mut writer, PAGE_POSITION);
    let calls = harness.drive_for(SETTLE);
    assert_eq!(player_seeks(&calls), vec![PAGE_POSITION]);
}

/// Эфир на T0 при загрузке не ставится: он начинается там, где лежат байты, а итоговое T0
/// исполняется перемоткой после показа (иначе плеер остался бы в начале истории).
#[test]
fn a_live_handoff_still_seeks_to_the_page_position_after_the_show() {
    let (mut harness, current, mut writer) = opening_generation(PushedDuration::Live);
    page_names_position(&mut harness, &current, &mut writer, PAGE_POSITION);
    harness.drive_until_loaded();
    assert_eq!(start_of_load(&harness, 0), None);

    page_names_position(&mut harness, &current, &mut writer, PAGE_POSITION);
    harness.show_last_load(Duration::ZERO);
    let mut after_show = harness.player.take_calls();
    after_show.extend(harness.drive_for(SETTLE));
    assert_eq!(player_seeks(&after_show), vec![PAGE_POSITION]);
}
