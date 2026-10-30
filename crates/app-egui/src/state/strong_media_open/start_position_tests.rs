//! Кто решает место старта: обычный плеер (цель восстановления) против владельца
//! источника (поколение браузерного потока, 04a).
//!
//! Живой дефект 04a: окно браузерного режима ставило поколение с позицией страницы
//! путём `AppState`, и договор «готовая позиция только для цели восстановления»
//! отвергал его — старое поколение играло дальше. Эти тесты закрепляют обе стороны:
//! поколение принимается и после установки только усыновляется (без второй
//! перемотки), а обычный плеер с готовой позицией без цели по-прежнему отказ.

use std::time::Duration;

use media_core::{DemuxSeekResult, Demuxer, MediaDuration, MediaTime};
use player_core::{
    InstalledPositionRestore, PreparedDemuxSeekLandingPolicy, PreparedInitialPosition,
    PreparedMedia,
};

use super::super::pending::{PostInstalledStartupPositionFacts, PostInstalledStartupPositionPlan};
use super::super::{PreparedPositionRestoreContractError, PreparedPositionRestoreStrategy};
use super::*;

/// Позиция страницы, на которую поставлено поколение (как в записи 03e).
const PAGE_POSITION: Duration = Duration::from_millis(3_500);

/// Готовая позиция: демуксер сел на ключевой кадр до цели, плеер декодирует вперёд.
fn positioned_at(target: Duration) -> PreparedInitialPosition {
    let target = MediaTime::from_duration(target);
    PreparedInitialPosition::PositionedAt {
        target_position: target,
        landing_policy: PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        result: DemuxSeekResult {
            requested_position: target,
            actual_position: target.saturating_sub(MediaDuration::from_millis(1_200)),
            actual_track_timestamp: None,
        },
    }
}

/// Шкала поколения: перематываемая, позиция цели почти в конце ролика — у обычного
/// возобновления здесь сработала бы политика «у конца — с начала».
fn facts_near_the_end() -> PostInstalledStartupPositionFacts {
    PostInstalledStartupPositionFacts::static_seekable(
        Some(PAGE_POSITION + Duration::from_secs(1)),
        PAGE_POSITION,
    )
}

/// Демуксер без дорожек: здесь проверяется только приём открытия, байты не читаются.
struct EmptyDemuxer;

impl Demuxer for EmptyDemuxer {
    fn tracks(&self) -> &[media_core::TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        None
    }

    fn next_event(&mut self) -> anyhow::Result<media_core::DemuxReadEvent> {
        Ok(media_core::DemuxReadEvent::EndOfStream)
    }

    fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        Err(anyhow::anyhow!("the empty test demuxer does not seek"))
    }
}

/// Медиа поколения, уже поставленное владельцем источника на позицию страницы.
fn browser_generation_media(position: PreparedInitialPosition) -> PreparedMedia {
    PreparedMedia::from_external_label("browser stream", Box::new(EmptyDemuxer))
        .with_prepared_initial_position(position)
        .expect("the test position is consistent")
}

fn browser_stream_label() -> SafeMediaLabel {
    SafeMediaLabel::from_service_safe_label("browser stream")
}

#[test]
fn a_source_positioned_generation_is_admitted_and_only_adopted_after_install() {
    let open = PreparedSingleMediaOpen::prepared_by_source(
        browser_generation_media(positioned_at(PAGE_POSITION)),
        ActiveMediaSource::BrowserStream,
        browser_stream_label(),
    );

    let plan = open
        .admit_start_position()
        .expect("the source owner's position is authoritative");

    assert_eq!(
        plan,
        PendingStartPositionPlan::AdoptSourcePrepared {
            target: PAGE_POSITION
        }
    );
    // После установки — ровно одно усыновление той же цели: ни перемотки демуксера
    // (`SeekTo`), ни отката к началу у конца ролика, ни предупреждения о позиции.
    assert_eq!(
        plan.post_installed_plan(facts_near_the_end()),
        PostInstalledStartupPositionPlan::Restore {
            requested_position: PAGE_POSITION,
            position: InstalledPositionRestore::AdoptPreparedInitialPosition {
                expected_target: PAGE_POSITION,
            },
        }
    );
}

#[test]
fn a_source_generation_from_its_beginning_needs_nothing_after_install() {
    let open = PreparedSingleMediaOpen::prepared_by_source(
        browser_generation_media(PreparedInitialPosition::Beginning),
        ActiveMediaSource::BrowserStream,
        browser_stream_label(),
    );

    let plan = open
        .admit_start_position()
        .expect("a beginning start is always admitted");

    assert_eq!(
        plan.post_installed_plan(facts_near_the_end()),
        PostInstalledStartupPositionPlan::Continue { warning: None }
    );
}

#[test]
fn an_ordinary_open_with_a_prepared_position_but_no_restore_target_is_still_rejected() {
    // Тот же медиа-объект, но позицию решает приложение (Open файла / очередь).
    let open = PreparedSingleMediaOpen::new(
        browser_generation_media(positioned_at(PAGE_POSITION)),
        ActiveMediaSource::BrowserStream,
        browser_stream_label(),
    );

    assert_eq!(
        open.admit_start_position(),
        Err(PreparedPositionRestoreContractError::MissingRestoreTarget)
    );
}

#[test]
fn the_queue_restore_contract_is_unchanged_for_matching_and_different_targets() {
    // Совпадающая цель восстановления (native HLS) — усыновление этой цели.
    let matching = PendingStartPositionPlan::admit(
        positioned_at(PAGE_POSITION),
        PreparedOpenStartPosition::Startup(StartupPosition::Restore(PAGE_POSITION)),
    )
    .expect("the restore target matches the player proof");
    assert_eq!(
        matching,
        PendingStartPositionPlan::Startup {
            position: StartupPosition::Restore(PAGE_POSITION),
            strategy: PreparedPositionRestoreStrategy::AdoptPreparedInitialPosition,
        }
    );
    // Другая цель — отказ, как и прежде.
    let other_target = Duration::from_secs(180);
    assert_eq!(
        PendingStartPositionPlan::admit(
            positioned_at(PAGE_POSITION),
            PreparedOpenStartPosition::Startup(StartupPosition::RestoreExact(other_target)),
        ),
        Err(PreparedPositionRestoreContractError::TargetMismatch {
            prepared_target: PAGE_POSITION,
            restore_target: other_target,
        })
    );
}

#[test]
fn the_plain_open_and_the_queue_transitions_keep_seeking_after_install() {
    // Путь без готовой позиции (переходы очереди) не меняет поведения: начало —
    // ничего, позиция Undo Clear — обычная перемотка после установки.
    let facts = PostInstalledStartupPositionFacts::static_seekable(
        Some(Duration::from_secs(600)),
        Duration::ZERO,
    );
    assert_eq!(
        PendingStartPositionPlan::seek_after_install(StartupPosition::KeepStart)
            .post_installed_plan(facts),
        PostInstalledStartupPositionPlan::Continue { warning: None }
    );
    assert_eq!(
        PendingStartPositionPlan::seek_after_install(StartupPosition::RestoreExact(PAGE_POSITION))
            .post_installed_plan(facts),
        PostInstalledStartupPositionPlan::Restore {
            requested_position: PAGE_POSITION,
            position: InstalledPositionRestore::SeekTo(PAGE_POSITION),
        }
    );
}
