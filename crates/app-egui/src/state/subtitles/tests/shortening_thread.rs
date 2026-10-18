//! Укорочение конца события на живом потоке-рисовальщике (открытая реплика закрыта преемником).
//!
//! Проверяется то, что видит зритель, и цена для потока: показанная картинка исчезает ровно на
//! новом конце, поток не получает полной копии дорожки (`Replace`), а картинка, которая и после
//! укорочения та же, не рисуется заново (нет мигания).

use subtitle_core::{SubtitleEventId, SubtitleSyncOffset, SubtitleTime};
use subtitle_raster::CueSourceId;

use super::super::active_track::ActiveSubtitleTrack;
use super::super::presentation::SubtitlePresenter;
use super::super::presentation::test_support::PumpMode;
use super::render_thread::{
    GENERATION, HEIGHT, WIDTH, cue_events, presenter_with_track, subtitle_event,
};
use super::*;

/// Одна «открытая» реплика (id 0) с 1 с почти на час: модель PGS-картинки до прихода преемника.
const OPEN_CUE_SRT: &str = "1\n00:00:01,000 --> 00:59:00,000\nOpen cue\n";

/// Открытая реплика (id 0) и короткая реплика F (id 1) на 3..4 с: заставляет картинки делиться
/// по времени, чтобы были картинки «до» и «после» нового конца.
const OPEN_AND_SHORT_SRT: &str = "1\n00:00:01,000 --> 00:59:00,000\nOpen cue\n\n\
                                  2\n00:00:03,000 --> 00:00:04,000\nShort cue\n";

fn ms(value: i64) -> SubtitleTime {
    SubtitleTime::from_millis(value)
}

/// Id реплики с порядковым номером `index` (конструктор id закрыт, берём из таймлайна).
fn event_id(track: &ActiveSubtitleTrack, index: usize) -> SubtitleEventId {
    track
        .timeline()
        .events_from_index(index)
        .next()
        .map(|(id, _)| id)
        .expect("event exists")
}

/// Реплики на экране в `pts_ms` (пусто - ничего не показано).
fn shown_at(
    presenter: &mut SubtitlePresenter,
    track: &ActiveSubtitleTrack,
    pts_ms: u64,
) -> Vec<u32> {
    presenter
        .overlay_for_frame(track, &frame(pts_ms, GENERATION, full_rect(WIDTH, HEIGHT)))
        .map(cue_events)
        .unwrap_or_default()
}

/// Сколько полных копий дорожки и узких дельт принял поток.
fn mirror_traffic(presenter: &mut SubtitlePresenter) -> (u64, u64) {
    let diagnostics = presenter.worker_diagnostics().expect("worker is running");
    (diagnostics.track_replaces, diagnostics.track_updates)
}

#[test]
fn open_event_shortened_before_the_shown_time_disappears_without_a_replace() {
    let (mut presenter, mut track) =
        presenter_with_track(CueSourceId::new(5), OPEN_CUE_SRT, SubtitleSyncOffset::ZERO);
    assert_eq!(shown_at(&mut presenter, &track, 2_000), vec![0]);
    assert_eq!(mirror_traffic(&mut presenter), (1, 0), "одна первая копия");

    // Преемник пришёл на 1.5 с: открытая реплика закрывается ДО показанного времени 2 с.
    track
        .shorten_event_end(event_id(&track, 0), ms(1_500))
        .expect("shorten");
    let after = shown_at(&mut presenter, &track, 2_000);

    assert_eq!(after, Vec::<u32>::new(), "реплика на 2 с больше не видна");
    assert_eq!(
        mirror_traffic(&mut presenter),
        (1, 1),
        "поток получил узкую дельту, а не новую копию дорожки"
    );
}

#[test]
fn picture_prepared_ahead_past_the_new_end_is_not_shown() {
    let (mut presenter, mut track) = presenter_with_track(
        CueSourceId::new(5),
        OPEN_AND_SHORT_SRT,
        SubtitleSyncOffset::ZERO,
    );
    // Картинки: [1 с, 3 с) с открытой репликой и - после прыжка вперёд - [3.5 с, 4 с) с обеими.
    assert_eq!(shown_at(&mut presenter, &track, 2_000), vec![0]);
    assert_eq!(shown_at(&mut presenter, &track, 3_500), vec![0, 1]);
    assert!(presenter.wait_for_idle());
    let calls_before = presenter.stats().rasterizer_calls;

    // Открытая реплика закрыта на 3.2 с: картинка 3.5 с (с ней) подготовлена впрок - она неверна.
    track
        .shorten_event_end(event_id(&track, 0), ms(3_200))
        .expect("shorten");

    assert_eq!(
        shown_at(&mut presenter, &track, 3_500),
        vec![1],
        "на 3.5 с осталась только короткая реплика"
    );
    assert!(presenter.wait_for_idle());
    assert_eq!(
        presenter.stats().rasterizer_calls,
        calls_before + 1,
        "перерисована ровно одна картинка - неверная картинка 3.5 с"
    );
    assert_eq!(mirror_traffic(&mut presenter), (1, 1));
}

#[test]
fn shortening_an_event_that_is_not_on_screen_does_not_re_render_the_shown_overlay() {
    let (mut presenter, mut track) = presenter_with_track(
        CueSourceId::new(5),
        "1\n00:00:01,000 --> 00:59:00,000\nOpen cue\n\n\
         2\n00:00:10,000 --> 00:00:20,000\nLater cue\n",
        SubtitleSyncOffset::ZERO,
    );
    assert_eq!(shown_at(&mut presenter, &track, 2_000), vec![0]);
    assert!(presenter.wait_for_idle());
    let calls_before = presenter.stats().rasterizer_calls;
    let raster_before = presenter.rasterizer_statistics().expect("worker");

    // Реплика 10..20 с укорочена до 15 с: показанная на 2 с картинка (до 10 с) её не касается.
    track
        .shorten_event_end(event_id(&track, 1), ms(15_000))
        .expect("shorten");
    assert_eq!(shown_at(&mut presenter, &track, 2_000), vec![0]);
    assert!(presenter.wait_for_idle());

    assert_eq!(presenter.stats().rasterizer_calls, calls_before);
    assert_eq!(
        presenter
            .rasterizer_statistics()
            .expect("worker")
            .rendered_frames,
        raster_before.rendered_frames
    );
    assert_eq!(mirror_traffic(&mut presenter), (1, 1));
}

#[test]
fn picture_before_the_new_end_keeps_showing_without_any_extra_render() {
    let (mut presenter, mut track) =
        presenter_with_track(CueSourceId::new(5), OPEN_CUE_SRT, SubtitleSyncOffset::ZERO);
    assert_eq!(shown_at(&mut presenter, &track, 2_000), vec![0]);
    assert!(presenter.wait_for_idle());
    let calls_before = presenter.stats().rasterizer_calls;
    let raster_before = presenter.rasterizer_statistics().expect("worker");

    track
        .shorten_event_end(event_id(&track, 0), ms(5_000))
        .expect("shorten");

    // Каждый кадр до нового конца показывает ту же реплику, без единой новой растеризации.
    for pts_ms in (2_000..5_000).step_by(40) {
        assert_eq!(
            shown_at(&mut presenter, &track, pts_ms),
            vec![0],
            "кадр {pts_ms} мс до нового конца"
        );
    }
    assert!(presenter.wait_for_idle());
    assert_eq!(presenter.stats().rasterizer_calls, calls_before);
    assert_eq!(
        presenter
            .rasterizer_statistics()
            .expect("worker")
            .rendered_frames,
        raster_before.rendered_frames,
        "нет ни одного лишнего кадра растеризатора"
    );
    // На новом конце реплики нет.
    assert_eq!(shown_at(&mut presenter, &track, 5_000), Vec::<u32>::new());
}

#[test]
fn shortening_together_with_an_append_shows_the_successor_at_the_boundary() {
    let (mut presenter, mut track) =
        presenter_with_track(CueSourceId::new(5), OPEN_CUE_SRT, SubtitleSyncOffset::ZERO);
    assert_eq!(shown_at(&mut presenter, &track, 2_000), vec![0]);

    // Преемник (id 1) приходит на 2.4 с, предшественник закрывается в тот же момент.
    track
        .shorten_event_end(event_id(&track, 0), ms(2_400))
        .expect("shorten");
    track.append_events([subtitle_event(2_400, 3_600_000, "Successor")]);

    assert_eq!(shown_at(&mut presenter, &track, 2_000), vec![0]);
    assert_eq!(shown_at(&mut presenter, &track, 2_440), vec![1]);
    assert_eq!(
        mirror_traffic(&mut presenter),
        (1, 1),
        "укорочение и дописывание ушли одной дельтой"
    );
}

#[test]
fn overflowed_shortening_history_falls_back_to_one_full_replace_and_still_shows_correctly() {
    // 1100 событий: i-е на [100 + i, 100 + i + 100000) мс, т.е. все «открытые».
    let (mut presenter, mut track) = presenter_with_track(
        CueSourceId::new(5),
        "1\n00:00:00,100 --> 00:10:00,000\nFirst\n",
        SubtitleSyncOffset::ZERO,
    );
    let many: Vec<_> = (1..1_100_i64)
        .map(|index| subtitle_event(100 + index, 600_000, &format!("Cue {index}")))
        .collect();
    track.append_events(many);
    assert_eq!(shown_at(&mut presenter, &track, 100), vec![0]);
    assert_eq!(mirror_traffic(&mut presenter), (1, 0));

    // Все события укорочены до одной миллисекунды: больше, чем помнит журнал таймлайна.
    for index in 0..1_100_usize {
        let start = 100 + i64::try_from(index).expect("small");
        track
            .shorten_event_end(event_id(&track, index), ms(start + 1))
            .expect("shorten");
    }

    assert_eq!(
        shown_at(&mut presenter, &track, 600),
        vec![500],
        "на 0.6 с виден только 500-й"
    );
    assert_eq!(
        mirror_traffic(&mut presenter),
        (2, 0),
        "журнал укорочений переполнен: страховка - ещё одна полная копия"
    );
}

/// Поток начинает рисовать открытую реплику (рисование занимает 300 мс), и пока он рисует,
/// UI узнаёт об укорочении. Возвращает презентатор с картинкой «в пути».
fn presenter_with_picture_in_flight() -> (SubtitlePresenter, ActiveSubtitleTrack) {
    let (mut presenter, track) =
        presenter_with_track(CueSourceId::new(5), OPEN_CUE_SRT, SubtitleSyncOffset::ZERO);
    presenter.set_pump_mode(PumpMode::FreeRunning);
    presenter.set_render_delay(std::time::Duration::from_millis(300));
    presenter.overlay_for_frame(&track, &frame(2_000, GENERATION, full_rect(WIDTH, HEIGHT)));
    // Даём потоку войти в рисование по старому состоянию дорожки.
    std::thread::sleep(std::time::Duration::from_millis(100));
    (presenter, track)
}

#[test]
fn picture_in_flight_that_contains_the_removed_span_is_dropped_on_arrival() {
    let (mut presenter, mut track) = presenter_with_picture_in_flight();

    // Реплика закрыта на 1.5 с: картинка, которую поток рисует для 2 с, содержит её зря.
    track
        .shorten_event_end(event_id(&track, 0), ms(1_500))
        .expect("shorten");
    presenter.overlay_for_frame(&track, &frame(2_040, GENERATION, full_rect(WIDTH, HEIGHT)));
    assert!(presenter.wait_for_idle());

    assert_eq!(shown_at(&mut presenter, &track, 2_040), Vec::<u32>::new());
    assert!(presenter.stats().dropped_outdated_pictures >= 1);
}

#[test]
fn picture_in_flight_drawn_before_the_new_end_is_kept_with_a_narrowed_validity() {
    let (mut presenter, mut track) = presenter_with_picture_in_flight();

    track
        .shorten_event_end(event_id(&track, 0), ms(5_000))
        .expect("shorten");
    presenter.overlay_for_frame(&track, &frame(2_040, GENERATION, full_rect(WIDTH, HEIGHT)));
    assert!(presenter.wait_for_idle());

    assert_eq!(shown_at(&mut presenter, &track, 2_040), vec![0]);
    assert_eq!(
        presenter.stats().dropped_outdated_pictures,
        0,
        "картинка до нового конца принята, а не выброшена"
    );
    assert_eq!(
        presenter.stats().rasterizer_calls,
        1,
        "и не рисовалась заново"
    );
    // Срок обрезан: на новом конце реплики на экране нет.
    assert_eq!(shown_at(&mut presenter, &track, 5_000), Vec::<u32>::new());
}
