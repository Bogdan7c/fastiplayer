//! Юнит-тесты склейки эфира (решение 333): решение «как продолжить окно» и новые снимки.

use std::time::Duration;

use hls_playlist_core::{
    HlsParseRequest, HlsParserLimits, HlsPlaylist, MediaPlaylist, parse_hls_playlist,
};
use source_core::HttpRequestTarget;

use super::*;
use crate::HlsRequiredContainer;
use crate::plan::{HlsComponentPlan, HlsEpochPlan};

/// Длительность одного сегмента и target duration синтетических окон.
const SEGMENT_SECS: u64 = 2;
/// Сегментов в окне: 8 × 2 с = 16 с, длиннее hold-back в 6 с.
const WINDOW_SEGMENTS: u64 = 8;

/// Скользящее окно из `WINDOW_SEGMENTS` сегментов с первой media sequence `first`.
fn window(first: u64, uri_prefix: &str) -> MediaPlaylist {
    let mut text =
        format!("#EXTM3U\n#EXT-X-TARGETDURATION:{SEGMENT_SECS}\n#EXT-X-MEDIA-SEQUENCE:{first}\n");
    for index in first..first + WINDOW_SEGMENTS {
        text.push_str(&format!(
            "#EXTINF:{SEGMENT_SECS}.0,\n{uri_prefix}{index}.ts\n"
        ));
    }
    let parsed = parse_hls_playlist(HlsParseRequest::new(
        text.as_bytes(),
        Some("https://live.example.invalid/stream/index.m3u8"),
        HlsParserLimits::default(),
    ))
    .expect("valid synthetic live playlist");
    let HlsPlaylist::Media(media) = parsed else {
        panic!("expected media playlist");
    };
    media
}

fn plan_for(media: &MediaPlaylist) -> HlsComponentPlan {
    let epochs = (0..media.segments.len() as u64)
        .map(|index| HlsEpochPlan {
            resources: Vec::new(),
            timeline_start: Duration::from_secs(index * SEGMENT_SECS),
        })
        .collect();
    HlsComponentPlan::test_without_media_resources(
        HlsRequiredContainer::TransportStream,
        epochs,
        Duration::from_secs(media.segments.len() as u64 * SEGMENT_SECS),
    )
}

fn target() -> HttpRequestTarget {
    HttpRequestTarget::parse_exact("https://live.example.invalid/stream/index.m3u8")
        .expect("valid synthetic target")
}

/// Скачанные плейлисты одной попытки: главный и, если задан, отдельный звук.
fn resources(main: MediaPlaylist, audio: Option<MediaPlaylist>) -> SelectedLiveResources {
    let main_plan = plan_for(&main);
    let audio_plan = audio.as_ref().map(plan_for);
    let has_audio = audio.is_some();
    SelectedLiveResources {
        main_media: main,
        main_plan,
        main_reload_target: target(),
        main_container: HlsRequiredContainer::TransportStream,
        main_track_layout: crate::HlsMainTrackLayoutIntent::VideoOnly,
        audio_media: audio,
        audio_plan,
        audio_reload_target: has_audio.then(target),
        audio_container: has_audio.then_some(HlsRequiredContainer::TransportStream),
        subtitles: Vec::new(),
        subtitle_base: target(),
    }
}

/// Снимки первого открытия: окно `first..` у главного и (опционально) у звука.
fn opened(
    main_first: u64,
    audio_first: Option<u64>,
) -> (HlsLiveComponentSnapshot, Option<HlsLiveComponentSnapshot>) {
    let initial = resources(
        window(main_first, "v"),
        audio_first.map(|first| window(first, "a")),
    );
    edge_aligned_snapshots(&initial, initial_shared_edge(&initial)).expect("valid initial window")
}

fn hold_back() -> Duration {
    Duration::from_secs(3 * SEGMENT_SECS)
}

/// Сегменты снимка стоят встык, без дыр и наложений.
fn assert_contiguous(snapshot: &HlsLiveComponentSnapshot) {
    for pair in snapshot.segments.windows(2) {
        assert_eq!(pair[0].timeline_end, pair[1].timeline_start);
    }
    assert_eq!(
        snapshot.segments.last().map(|segment| segment.timeline_end),
        Some(snapshot.manifest_live_edge)
    );
}

#[test]
fn a_window_without_any_known_segment_is_a_lost_thread_not_an_error() {
    let (main, _) = opened(100, None);
    // Пока машина спала, окно ушло на 100 сегментов вперёд: ни одного общего.
    let next = resources(window(200, "v"), None);

    let continuation =
        plan_refresh_continuation(&main, None, &next, HlsLiveManifestFreshness::AllFetched)
            .expect("lost thread is recoverable");

    assert!(matches!(
        continuation,
        HlsLiveRefreshContinuation::LostThread
    ));
}

#[test]
fn an_overlapping_window_keeps_sliding_on_the_old_timeline() {
    let (main, _) = opened(100, None);
    let next = resources(window(103, "v"), None);

    let HlsLiveRefreshContinuation::Continued { main: slid, audio } =
        plan_refresh_continuation(&main, None, &next, HlsLiveManifestFreshness::AllFetched)
            .expect("ordinary sliding refresh")
    else {
        panic!("an overlapping refresh must continue the window");
    };

    assert!(audio.is_none());
    // Сегмент 103 стоит там же, где стоял в старом окне: нить цела, склейки нет.
    let old_103 = main
        .segments
        .iter()
        .find(|segment| segment.identity.media_sequence == 103)
        .expect("segment 103 in the old window");
    assert_eq!(slid.segments[0].timeline_start, old_103.timeline_start);
    assert_eq!(
        slid.manifest_live_edge,
        main.manifest_live_edge + Duration::from_secs(3 * SEGMENT_SECS)
    );
}

#[test]
fn other_continuity_violations_stay_fatal() {
    let (main, _) = opened(100, None);

    // Нумерация пошла назад (перезапуск сервера эфира): решение 335 — честная ошибка.
    let restarted = resources(window(10, "v"), None);
    assert_eq!(
        plan_refresh_continuation(
            &main,
            None,
            &restarted,
            HlsLiveManifestFreshness::AllFetched
        )
        .err(),
        Some(HlsLiveRefreshError::SequenceRegressed)
    );

    // Сегмент с тем же номером сменил адрес: RFC велит остановиться.
    let mutated = resources(window(103, "other-"), None);
    assert_eq!(
        plan_refresh_continuation(&main, None, &mutated, HlsLiveManifestFreshness::AllFetched)
            .err(),
        Some(HlsLiveRefreshError::RetainedSegmentMutated)
    );
}

#[test]
fn a_reused_neighbour_playlist_is_never_used_to_rejoin() {
    let (main, audio) = opened(100, Some(500));
    // Главный плейлист свежий и без общего сегмента, звук взят из прошлой попытки.
    let next = resources(window(200, "v"), Some(window(500, "a")));

    let continuation = plan_refresh_continuation(
        &main,
        audio.as_ref(),
        &next,
        HlsLiveManifestFreshness::SomeReused,
    )
    .expect("stale neighbour is not an error");

    assert!(matches!(
        continuation,
        HlsLiveRefreshContinuation::NeedsFreshManifests
    ));
}

#[test]
fn a_component_that_lost_its_thread_alone_is_aligned_to_its_neighbour_edge() {
    let (main, audio) = opened(100, Some(500));
    // Звук ушёл далеко вперёд, главный компонент лишь сдвинулся на два сегмента.
    let next = resources(window(102, "v"), Some(window(600, "a")));

    let HlsLiveRefreshContinuation::Continued {
        main: slid,
        audio: Some(aligned_audio),
    } = plan_refresh_continuation(
        &main,
        audio.as_ref(),
        &next,
        HlsLiveManifestFreshness::AllFetched,
    )
    .expect("partial gap is recoverable")
    else {
        panic!("partial gap must continue with both components");
    };

    // Главный компонент сохранил точную старую шкалу, звук встал краем к его краю.
    assert_eq!(
        slid.manifest_live_edge,
        main.manifest_live_edge + Duration::from_secs(2 * SEGMENT_SECS)
    );
    assert_eq!(aligned_audio.manifest_live_edge, slid.manifest_live_edge);
    assert_contiguous(&aligned_audio);
}

#[test]
fn a_component_pairing_change_is_fatal() {
    let (main, _) = opened(100, None);
    let next = resources(window(200, "v"), Some(window(600, "a")));

    assert_eq!(
        plan_refresh_continuation(&main, None, &next, HlsLiveManifestFreshness::AllFetched).err(),
        Some(HlsLiveRefreshError::ComponentPairingChanged)
    );
}

#[test]
fn rejoin_places_the_new_join_point_exactly_where_reading_stopped() {
    // Старое окно кончалось на 16 с; демуксер дочитал до 40 с шкалы (эфир шёл долго).
    let resume_at = Duration::from_secs(40);
    let next = resources(window(200, "v"), None);

    let rejoined = rejoined_snapshots(&next, resume_at).expect("rejoin places the window");

    // Точка входа нового окна — там, где кончилось прочитанное: шкала без прыжка.
    assert_eq!(rejoined.join_position, resume_at);
    // Вход — по правилу первого открытия: ровно hold-back от нового края.
    assert_eq!(rejoined.main.manifest_live_edge - resume_at, hold_back());
    assert_contiguous(&rejoined.main);
    // Сегмент входа начинается ровно в точке продолжения, а не перекрывает её.
    let join_segment = first_segment_after(&rejoined.main, resume_at).expect("join segment");
    assert_eq!(join_segment.timeline_start, resume_at);
    assert_eq!(join_segment.identity.media_sequence, 205);
}

#[test]
fn rejoin_with_alternate_audio_keeps_one_join_for_both_components() {
    let resume_at = Duration::from_secs(40);
    let next = resources(window(200, "v"), Some(window(900, "a")));

    let rejoined = rejoined_snapshots(&next, resume_at).expect("joint rejoin");
    let audio = rejoined.audio.as_ref().expect("audio stays selected");

    assert_eq!(rejoined.join_position, resume_at);
    // Оба окна кончаются на одном крае (правило первого открытия).
    assert_eq!(audio.manifest_live_edge, rejoined.main.manifest_live_edge);
    assert_eq!(
        hold_back::join_position(&rejoined.main, Some(audio)),
        resume_at
    );
    assert_contiguous(audio);
}

#[test]
fn rejoin_before_the_unshifted_join_point_never_goes_below_zero() {
    // Эфир моложе своего окна: читали только до 2 с, а вход окна без сдвига — на 10 с.
    let resume_at = Duration::from_secs(2);
    let next = resources(window(200, "v"), None);

    let rejoined = rejoined_snapshots(&next, resume_at).expect("unshifted rejoin");

    assert_eq!(rejoined.main.segments[0].timeline_start, Duration::ZERO);
    assert_eq!(
        rejoined.join_position,
        Duration::from_secs((WINDOW_SEGMENTS - 3) * SEGMENT_SECS)
    );
    assert!(rejoined.join_position > resume_at);
}

#[test]
fn continuation_after_a_lost_segment_never_reads_backwards() {
    // Вход склейки — сегмент 205 на [40 с, 42 с); прочитано уже до 41 с, середины 205-го.
    let next = resources(window(200, "v"), None);
    let rejoined = rejoined_snapshots(&next, Duration::from_secs(40)).expect("rejoin");

    let next_segment =
        first_segment_after(&rejoined.main, Duration::from_secs(41)).expect("next segment");

    // Не 205 (начало 40 с < 41 с, метки пошли бы назад), а следующий за ним.
    assert_eq!(next_segment.identity.media_sequence, 206);
    assert!(next_segment.timeline_start >= Duration::from_secs(41));
}

#[test]
fn continuation_in_an_ordinary_sliding_window_is_the_adjacent_segment() {
    let (main, _) = opened(100, None);
    let read_until = main.segments[2].timeline_end;

    let next_segment = first_segment_after(&main, read_until).expect("next segment");

    // Сегменты стыкуются встык: продолжение — ровно соседний, как до 16d3.
    assert_eq!(next_segment.identity, main.segments[3].identity);
}

#[test]
fn continuation_behind_the_whole_window_takes_the_oldest_segment() {
    let (main, _) = opened(100, None);
    let slid = main
        .refreshed(&window(106, "v"), plan_for(&window(106, "v")))
        .expect("sliding refresh");

    // Демуксер отстал: прочитанное кончилось раньше начала окна.
    let next_segment = first_segment_after(&slid, Duration::from_secs(1)).expect("oldest");

    assert_eq!(next_segment.identity.media_sequence, 106);
}

#[test]
fn continuation_at_the_edge_has_nothing_to_read() {
    let (main, _) = opened(100, None);

    assert!(first_segment_after(&main, main.manifest_live_edge).is_none());
}

fn ended(mut media: MediaPlaylist) -> MediaPlaylist {
    media.end_list = true;
    media
}

#[test]
fn a_playlist_reused_from_the_last_attempt_is_stale_unless_it_has_ended() {
    let live_audio = resources(window(200, "v"), Some(window(900, "a")));
    let ended_audio = resources(window(200, "v"), Some(ended(window(900, "a"))));
    let main_only = crate::live::reload_schedule::HlsLiveDueRenditions {
        main: true,
        audio: false,
    };
    let everything = crate::live::reload_schedule::HlsLiveDueRenditions {
        main: true,
        audio: true,
    };

    // Звук не качали в этой попытке и он ещё в эфире: его окно устарело.
    assert_eq!(
        manifest_freshness(&live_audio, main_only),
        HlsLiveManifestFreshness::SomeReused
    );
    // Звук кончился `ENDLIST`: он больше не меняется, перекачивать нечего — петли нет.
    assert_eq!(
        manifest_freshness(&ended_audio, main_only),
        HlsLiveManifestFreshness::AllFetched
    );
    assert_eq!(
        manifest_freshness(&live_audio, everything),
        HlsLiveManifestFreshness::AllFetched
    );
}
