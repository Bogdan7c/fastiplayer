//! Hold-back: на каком расстоянии от края эфира live-воспроизведение входит в плейлист.
//!
//! RFC 8216 §6.3.3: клиент НЕ ДОЛЖЕН выбирать для старта сегмент, который начинается
//! ближе трёх target duration к концу плейлиста. Причина практическая: край эфира
//! обновляется рывками (сегмент появляется целиком раз в target duration), и плеер,
//! стартовавший «на самом краю», на каждом сегменте ждёт следующий с нулевым запасом.
//!
//! Модуль владеет двумя связанными решениями, которые обязаны совпадать:
//! 1. с какого сегмента demux начинает читать (`start_index_for_join`);
//! 2. с какой позиции стартуют часы плеера (`join_position`).
//!
//! Если часы стартуют на live edge, а demux читает раньше, плеер отрезает всё, что
//! раньше часов (см. trim по `media_clock_base`), и hold-back превращается в ноль:
//! именно это давало циклы Playing -> Buffering -> Playing на каждом сегменте (F3).

use std::time::Duration;

use super::HlsLiveComponentSnapshot;

/// Сколько target duration отступать от конца плейлиста при входе в эфир.
///
/// RFC 8216 §6.3.3 задаёт минимум именно три; меньше нельзя. Больше не берём: задержка
/// эфира растёт линейно (при target 6 с это уже 18 с), а запаса в три сегмента хватает,
/// чтобы пережить обычную неравномерность публикации сегментов.
/// `EXT-X-SERVER-CONTROL:HOLD-BACK` парсер сейчас пропускает; при его поддержке
/// значение сервера (оно по RFC не меньше трёх target duration) заменит эту константу.
const HOLD_BACK_TARGET_DURATIONS: u32 = 3;

/// Позиция таймлайна, не ближе которой к краю нужно начинать воспроизведение.
fn hold_back_target(snapshot: &HlsLiveComponentSnapshot) -> Duration {
    let hold_back = snapshot
        .target_duration
        .saturating_mul(HOLD_BACK_TARGET_DURATIONS);
    snapshot.manifest_live_edge.saturating_sub(hold_back)
}

/// Начало сегмента, с которого этот component входит в эфир по правилу hold-back.
///
/// Берётся последний сегмент, начинающийся не позже `hold_back_target`, то есть
/// расстояние до края не меньше hold-back (граница сегмента округляет «назад»).
/// Если плейлист короче hold-back, берётся самый старый сегмент: ближе к началу
/// уйти некуда. Для пустого списка сегментов возвращает live edge.
fn component_join_position(snapshot: &HlsLiveComponentSnapshot) -> Duration {
    let target = hold_back_target(snapshot);
    snapshot
        .segments
        .iter()
        .rev()
        .find(|segment| segment.timeline_start <= target)
        .or_else(|| snapshot.segments.first())
        .map_or(snapshot.manifest_live_edge, |segment| {
            segment.timeline_start
        })
}

/// Общая позиция входа в эфир для main и (опционально) отдельного audio rendition.
///
/// Берём максимум: часы стартуют не раньше, чем начинает отдавать данные самый
/// «поздний» из компонентов. Более ранний компонент просто обрежется по часам,
/// а в обратную сторону вышла бы тишина или чёрный кадр до прихода данных.
pub(super) fn join_position(
    main: &HlsLiveComponentSnapshot,
    audio: Option<&HlsLiveComponentSnapshot>,
) -> Duration {
    let main_join = component_join_position(main);
    audio.map_or(main_join, |audio| {
        main_join.max(component_join_position(audio))
    })
}

/// Индекс сегмента, с которого demux начинает чтение для заданной позиции входа.
///
/// Это сегмент, содержащий `join_position`. Если он уже вытеснен из окна (гонка
/// между публикацией позиции и открытием demux-а), начинаем с самого старого
/// сегмента: лучше короткая тишина в начале, чем отказ открыть эфир.
pub(super) fn start_index_for_join(
    snapshot: &HlsLiveComponentSnapshot,
    join_position: Duration,
) -> usize {
    snapshot
        .segments
        .iter()
        .rposition(|segment| segment.timeline_start <= join_position)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use hls_playlist_core::{
        HlsParseRequest, HlsParserLimits, HlsPlaylist, MediaPlaylist, parse_hls_playlist,
    };

    use super::*;
    use crate::HlsRequiredContainer;
    use crate::live::HlsLiveComponentKind;
    use crate::plan::{HlsComponentPlan, HlsEpochPlan};

    /// Скользящее окно из `count` сегментов по `segment_secs` с первой media sequence `first`.
    fn sliding_window(first: u64, count: u64, segment_secs: u64) -> MediaPlaylist {
        let mut text = format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:{segment_secs}\n#EXT-X-MEDIA-SEQUENCE:{first}\n"
        );
        for index in first..first + count {
            text.push_str(&format!("#EXTINF:{segment_secs}.0,\ns{index}.ts\n"));
        }
        let parsed = parse_hls_playlist(HlsParseRequest::new(
            text.as_bytes(),
            Some("https://live.example.invalid/radio/index.m3u8"),
            HlsParserLimits::default(),
        ))
        .expect("valid synthetic live playlist");
        let HlsPlaylist::Media(media) = parsed else {
            panic!("expected media playlist");
        };
        media
    }

    fn plan_for(media: &MediaPlaylist, segment_secs: u64) -> HlsComponentPlan {
        let epochs = (0..media.segments.len() as u64)
            .map(|index| HlsEpochPlan {
                resources: Vec::new(),
                timeline_start: Duration::from_secs(index * segment_secs),
            })
            .collect();
        HlsComponentPlan::test_without_media_resources(
            HlsRequiredContainer::TransportStream,
            epochs,
            Duration::from_secs(media.segments.len() as u64 * segment_secs),
        )
    }

    fn initial_snapshot(count: u64, segment_secs: u64) -> HlsLiveComponentSnapshot {
        let media = sliding_window(100, count, segment_secs);
        HlsLiveComponentSnapshot::initial(
            HlsLiveComponentKind::Main,
            &media,
            plan_for(&media, segment_secs),
            Duration::from_secs(count * segment_secs),
        )
        .expect("valid initial live snapshot")
    }

    /// RFC 8216 §6.3.3: вход не ближе трёх target duration к концу плейлиста.
    #[test]
    fn join_keeps_at_least_three_target_durations_from_the_edge() {
        for segment_secs in [2, 6] {
            let snapshot = initial_snapshot(8, segment_secs);
            let join = join_position(&snapshot, None);
            let edge = snapshot.manifest_live_edge;
            assert!(
                edge - join >= Duration::from_secs(3 * segment_secs),
                "target {segment_secs} s: join {join:?} слишком близко к краю {edge:?}"
            );
            // Не отодвигаемся дальше необходимого: ровно три сегмента от края.
            assert_eq!(edge - join, Duration::from_secs(3 * segment_secs));
        }
    }

    #[test]
    fn short_playlist_joins_at_oldest_segment() {
        // Окно 2 сегмента по 6 с (12 с) короче hold-back в 18 с: уходить дальше некуда.
        let snapshot = initial_snapshot(2, 6);
        assert_eq!(join_position(&snapshot, None), Duration::ZERO);
        assert_eq!(start_index_for_join(&snapshot, Duration::ZERO), 0);
    }

    #[test]
    fn demux_start_segment_contains_join_position() {
        let snapshot = initial_snapshot(8, 2);
        let join = join_position(&snapshot, None);
        let index = start_index_for_join(&snapshot, join);
        assert_eq!(snapshot.segments[index].timeline_start, join);
    }

    #[test]
    fn join_of_expired_segment_falls_back_to_oldest_retained() {
        let snapshot = initial_snapshot(5, 2);
        // Позиция левее окна: читаем с самого старого сегмента, а не отказываем.
        assert_eq!(start_index_for_join(&snapshot, Duration::ZERO), 0);
        let shifted = snapshot
            .refreshed(
                &sliding_window(103, 5, 2),
                plan_for(&sliding_window(103, 5, 2), 2),
            )
            .expect("sliding refresh");
        assert_eq!(start_index_for_join(&shifted, Duration::ZERO), 0);
    }

    /// Main и отдельный audio rendition входят в эфир по более позднему из двух стартов.
    #[test]
    fn join_with_alternate_audio_takes_the_later_component_start() {
        let main = initial_snapshot(8, 2);
        // Аудио с другой нумерацией и тем же edge: окно сдвинуто на целый сегмент вперёд.
        let audio_media = sliding_window(200, 8, 2);
        let audio = HlsLiveComponentSnapshot::initial(
            HlsLiveComponentKind::AlternateAudio,
            &audio_media,
            plan_for(&audio_media, 2),
            Duration::from_secs(16),
        )
        .expect("valid audio snapshot");
        let joint = join_position(&main, Some(&audio));
        assert_eq!(joint, Duration::from_secs(10));
        // Оба компонента могут стартовать с сегмента, содержащего общую позицию.
        assert_eq!(start_index_for_join(&main, joint), 5);
        assert_eq!(start_index_for_join(&audio, joint), 5);
    }

    /// Живое окно ffmpeg (5 сегментов) и сегменты строго по расписанию: 60 с после входа
    /// данные впереди часов не заканчиваются. Часы идут в реальном темпе, а новый сегмент
    /// появляется ровно раз в target duration. Без hold-back (вход на краю) запас в
    /// момент публикации каждого сегмента равен нулю: это и был цикл Buffering (F3).
    #[test]
    fn schedule_keeps_data_ahead_of_clock_for_a_minute() {
        const SEGMENT_SECS: u64 = 2;
        let mut window = initial_snapshot(5, SEGMENT_SECS);
        let start_edge = window.manifest_live_edge;
        let join = join_position(&window, None);
        let mut first_media_sequence = 100;

        let mut min_runway_with_hold_back = Duration::MAX;
        let mut min_runway_at_edge = Duration::MAX;
        for elapsed_secs in 1..=60u64 {
            // Сервер публикует очередной сегмент каждые SEGMENT_SECS секунд.
            if elapsed_secs % SEGMENT_SECS == 0 {
                first_media_sequence += 1;
                let media = sliding_window(first_media_sequence, 5, SEGMENT_SECS);
                window = window
                    .refreshed(&media, plan_for(&media, SEGMENT_SECS))
                    .expect("sliding refresh in schedule");
            }
            let available_until = window.manifest_live_edge;
            let clock = join + Duration::from_secs(elapsed_secs);
            let edge_clock = start_edge + Duration::from_secs(elapsed_secs);
            min_runway_with_hold_back =
                min_runway_with_hold_back.min(available_until.saturating_sub(clock));
            min_runway_at_edge = min_runway_at_edge.min(available_until.saturating_sub(edge_clock));
        }

        assert_eq!(
            min_runway_at_edge,
            Duration::ZERO,
            "модель должна ловить старый баг"
        );
        assert!(
            min_runway_with_hold_back >= Duration::from_secs(2 * SEGMENT_SECS),
            "запас {min_runway_with_hold_back:?} меньше двух сегментов"
        );
    }
}
