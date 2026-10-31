//! Чистые правила MPRIS окна браузерного режима: что делать с командой с шины и как
//! выглядит публикуемый вид. Ничего не знает ни про D-Bus-поток, ни про `AppState`.
//!
//! Зачем отдельно: правила — это «политика» (какие кнопки медиа-виджета окну доступны,
//! во что превращается `Stop`), и их удобно закреплять тестами без окна и шины.

use std::time::Duration;

use desktop_integration::{
    DesktopCapabilities, DesktopCommand, DesktopControlRevision, DesktopMetadata,
    DesktopPlaybackStatus, DesktopSnapshotRevision, DesktopSnapshotView, DesktopTrackKey,
    DesktopTransportAction, EffectiveVolume,
};
use media_core::{MediaDuration, MediaTime};
use player_core::{PlaybackState, PlayerCommand, SeekRequest};

use crate::page_label::PageLabel;

/// Страница, которую сейчас показывает окно, с номером «видео в окне».
///
/// `lineage` растёт с каждым новым видео (старт сеанса, замена вкладки): по нему шина
/// отличает команды, адресованные уже ушедшему видео (`control_revision` и ключ трека).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PageInfo {
    pub(super) lineage: u64,
    pub(super) label: PageLabel,
}

impl PageInfo {
    /// Ключ трека в D-Bus: внешнее медиа с номером видео в окне.
    fn track_key(&self) -> DesktopTrackKey {
        DesktopTrackKey::ExternalMedia {
            lineage: self.lineage,
        }
    }

    /// «Ревизия управления»: команды, увиденные виджетом при другом видео, отбрасываются.
    fn control_revision(&self) -> DesktopControlRevision {
        DesktopControlRevision::new(self.lineage)
    }
}

/// Что окно видит в плеере (то, что нужно MPRIS), без ссылок на `PlayerSnapshot`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TransportObservation {
    /// Состояние плеера.
    pub(crate) playback_state: PlaybackState,
    /// Позиция на таймлайне.
    pub(crate) position: MediaTime,
    /// Длительность, если известна (эфир и неизвестное — `None`).
    pub(crate) duration: Option<MediaDuration>,
    /// Умеет ли таймлайн перемотку.
    pub(crate) seekable: bool,
    /// Громкость плеера, 0..=1.
    pub(crate) volume: f32,
}

/// Почему команда с шины не дошла до окна.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IgnoredReason {
    /// Окну ещё не назвали страницу: управлять нечем.
    NoPageYet,
    /// Команду выдал виджет, видевший прошлое видео окна.
    StaleControl,
    /// `SetPosition` с ключом трека другого видео.
    StaleTrack,
    /// Очереди в этом окне нет (Next/Previous/Loop/Shuffle).
    NoQueueInThisWindow,
}

/// Решение по команде с шины.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum PlannedAction {
    /// Исполнить так же, как кнопку окна: через `AppState::send_transport_command`.
    Player(PlayerCommand),
    /// Не исполнять.
    Ignored(IgnoredReason),
}

/// Превращает команду шины в команду транспорта окна.
///
/// - `Stop` = пауза: в окне один источник, «остановить» ему нечего (выгрузить видео =
///   закрыть окно, это `Quit`); пауза — безопасный смысл кнопки «стоп» медиа-виджета.
/// - `SetRatePause` (виджет просит скорость 0) = пауза, как у обычного плеера; скорость
///   страницы с шины не меняется (MPRIS `Rate` зафиксирован в 1.0).
/// - `Seek` — относительный: цель считаем здесь от позиции окна и зажимаем в
///   `[0; длительность]` (обычный плеер за концом уходит к следующему, у окна его нет).
/// - команды, зависящие от видео, сверяются с ревизией управления (как у обычного
///   плеера); `Stop` и громкость не привязаны к видео.
pub(super) fn plan_command(
    command: &DesktopCommand,
    page: Option<&PageInfo>,
    observation: &TransportObservation,
) -> PlannedAction {
    use DesktopTransportAction as Action;
    match &command.action {
        Action::Next | Action::Previous | Action::SetLoopStatus(_) | Action::SetShuffle(_) => {
            PlannedAction::Ignored(IgnoredReason::NoQueueInThisWindow)
        }
        Action::Stop => PlannedAction::Player(PlayerCommand::Pause),
        Action::SetVolume(volume) => {
            PlannedAction::Player(PlayerCommand::SetVolume(volume.as_player()))
        }
        Action::Play | Action::Pause | Action::PlayPause | Action::SetRatePause => {
            match fenced_page(command, page) {
                Ok(_) => PlannedAction::Player(match &command.action {
                    Action::Play => PlayerCommand::Play,
                    Action::PlayPause => PlayerCommand::TogglePlayback,
                    _ => PlayerCommand::Pause,
                }),
                Err(reason) => PlannedAction::Ignored(reason),
            }
        }
        Action::Seek {
            offset_microseconds,
            ..
        } => match fenced_page(command, page) {
            Ok(_) => PlannedAction::Player(PlayerCommand::Seek(SeekRequest::absolute(
                relative_target(observation, *offset_microseconds),
            ))),
            Err(reason) => PlannedAction::Ignored(reason),
        },
        Action::SetPosition {
            track_key,
            position_microseconds,
            ..
        } => match fenced_page(command, page) {
            Ok(page) if page.track_key() == *track_key => {
                PlannedAction::Player(PlayerCommand::Seek(SeekRequest::absolute(
                    microseconds_to_time(*position_microseconds),
                )))
            }
            Ok(_) => PlannedAction::Ignored(IgnoredReason::StaleTrack),
            Err(reason) => PlannedAction::Ignored(reason),
        },
    }
}

/// Страница, если команда видела именно это видео окна.
fn fenced_page<'page>(
    command: &DesktopCommand,
    page: Option<&'page PageInfo>,
) -> Result<&'page PageInfo, IgnoredReason> {
    let page = page.ok_or(IgnoredReason::NoPageYet)?;
    if command.observed_control_revision == Some(page.control_revision()) {
        Ok(page)
    } else {
        Err(IgnoredReason::StaleControl)
    }
}

/// Цель относительной перемотки: позиция окна + смещение, зажатая в `[0; длительность]`.
fn relative_target(observation: &TransportObservation, offset_microseconds: i64) -> MediaTime {
    let current =
        i128::try_from(observation.position.as_duration().as_micros()).unwrap_or(i128::MAX);
    let mut target = current
        .saturating_add(i128::from(offset_microseconds))
        .max(0);
    if let Some(duration) = observation.duration {
        let limit = i128::try_from(duration.as_duration().as_micros()).unwrap_or(i128::MAX);
        target = target.min(limit);
    }
    microseconds_to_time(i64::try_from(target).unwrap_or(i64::MAX))
}

/// Микросекунды MPRIS → время медиа (отрицательное — ноль).
fn microseconds_to_time(microseconds: i64) -> MediaTime {
    let microseconds = u64::try_from(microseconds).unwrap_or(0);
    MediaTime::from_duration(Duration::from_micros(microseconds))
}

/// Состояние плеера → `PlaybackStatus` MPRIS (то же отображение, что у обычного плеера).
fn playback_status(state: PlaybackState) -> DesktopPlaybackStatus {
    match state {
        PlaybackState::Playing
        | PlaybackState::Buffering
        | PlaybackState::Seeking
        | PlaybackState::Draining => DesktopPlaybackStatus::Playing,
        PlaybackState::Paused => DesktopPlaybackStatus::Paused,
        PlaybackState::Idle
        | PlaybackState::Opening
        | PlaybackState::Scrubbing
        | PlaybackState::Ended
        | PlaybackState::Stopped
        | PlaybackState::Failed => DesktopPlaybackStatus::Stopped,
    }
}

/// Вид для публикации. Без страницы — нейтральный (остановлено, кнопок нет).
///
/// Метаданные: `xesam:title` — заголовок страницы (пустой — домен), `xesam:album` —
/// домен. MPRIS живёт на шине сеансов, на диск его не пишет сам; человек, у которого
/// медиа-виджет, видит там «что играет» — это и есть его назначение.
/// Next/Previous всегда недоступны.
pub(super) fn build_view(
    revision: u64,
    page: Option<&PageInfo>,
    observation: &TransportObservation,
) -> DesktopSnapshotView {
    let volume = EffectiveVolume::from_player(observation.volume).unwrap_or(EffectiveVolume::FULL);
    let mut view = DesktopSnapshotView::neutral(volume);
    view.revision = DesktopSnapshotRevision::new(revision);
    view.position = observation.position;
    let Some(page) = page else {
        return view;
    };
    view.control_revision = Some(page.control_revision());
    view.playback_status = playback_status(observation.playback_state);
    view.capabilities = DesktopCapabilities {
        can_go_next: false,
        can_go_previous: false,
        can_play: true,
        can_pause: true,
        can_seek: observation.seekable,
    };
    view.metadata = build_metadata(page, observation.duration);
    view
}

/// Метаданные страницы (см. [`build_view`]).
fn build_metadata(page: &PageInfo, duration: Option<MediaDuration>) -> DesktopMetadata {
    let title = page.label.title().trim();
    let domain = page.label.domain();
    let (title, collection_context) = if title.is_empty() {
        (domain.to_owned(), None)
    } else {
        (title.to_owned(), Some(domain.to_owned()))
    };
    DesktopMetadata {
        track_key: Some(page.track_key()),
        title: Some(title),
        collection_context,
        source_label: None,
        duration,
    }
}

/// Вид без ревизии и позиции: если он равен прошлому, полная публикация не нужна.
pub(super) fn differs_beyond_position(
    previous: &DesktopSnapshotView,
    next: &DesktopSnapshotView,
) -> bool {
    let mut aligned = next.clone();
    aligned.revision = previous.revision;
    aligned.position = previous.position;
    aligned != *previous
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use desktop_integration::{DesktopCommandRequestId, TimelineSeekRequestId};

    use super::*;
    use crate::viewer_transport::{ViewerTransportIntent, viewer_intent_for_command};

    fn page(lineage: u64) -> PageInfo {
        PageInfo {
            lineage,
            label: PageLabel::new("Cats", "video.example"),
        }
    }

    fn observation() -> TransportObservation {
        TransportObservation {
            playback_state: PlaybackState::Playing,
            position: MediaTime::from_secs(30),
            duration: Some(MediaDuration::from_duration(Duration::from_secs(100))),
            seekable: true,
            volume: 0.5,
        }
    }

    fn command(action: DesktopTransportAction, revision: Option<u64>) -> DesktopCommand {
        DesktopCommand {
            request_id: DesktopCommandRequestId::new(NonZeroU64::MIN),
            observed_control_revision: revision.map(DesktopControlRevision::new),
            action,
        }
    }

    fn seek_id() -> TimelineSeekRequestId {
        TimelineSeekRequestId::new(NonZeroU64::MIN)
    }

    /// Команда с шины → команда окна → намерение зрителя в ящике, ровно как у кнопки.
    fn intent_of(action: DesktopTransportAction) -> Option<ViewerTransportIntent> {
        let observed = observation();
        let planned = plan_command(&command(action, Some(3)), Some(&page(3)), &observed);
        let PlannedAction::Player(player_command) = planned else {
            return None;
        };
        viewer_intent_for_command(
            &player_command,
            observed.playback_state,
            Duration::from_secs(30),
        )
    }

    #[test]
    fn play_pause_and_toggle_become_the_viewer_intents_of_the_window_buttons() {
        assert_eq!(
            intent_of(DesktopTransportAction::Play),
            Some(ViewerTransportIntent::Play)
        );
        assert_eq!(
            intent_of(DesktopTransportAction::Pause),
            Some(ViewerTransportIntent::Pause)
        );
        // Играет → переключатель даёт паузу.
        assert_eq!(
            intent_of(DesktopTransportAction::PlayPause),
            Some(ViewerTransportIntent::Pause)
        );
        assert_eq!(
            intent_of(DesktopTransportAction::SetRatePause),
            Some(ViewerTransportIntent::Pause)
        );
    }

    #[test]
    fn stop_is_a_pause_and_needs_no_fence() {
        let planned = plan_command(
            &command(DesktopTransportAction::Stop, None),
            None,
            &observation(),
        );
        assert_eq!(planned, PlannedAction::Player(PlayerCommand::Pause));
    }

    #[test]
    fn relative_seek_is_resolved_from_the_window_position_and_clamped() {
        let seek = |offset| DesktopTransportAction::Seek {
            request_id: seek_id(),
            offset_microseconds: offset,
        };
        assert_eq!(
            intent_of(seek(10_000_000)),
            Some(ViewerTransportIntent::Seek(Duration::from_secs(40)))
        );
        assert_eq!(
            intent_of(seek(-500_000_000)),
            Some(ViewerTransportIntent::Seek(Duration::ZERO))
        );
        assert_eq!(
            intent_of(seek(900_000_000)),
            Some(ViewerTransportIntent::Seek(Duration::from_secs(100)))
        );
    }

    #[test]
    fn set_position_seeks_only_the_current_video() {
        let set = |lineage| DesktopTransportAction::SetPosition {
            request_id: seek_id(),
            track_key: DesktopTrackKey::ExternalMedia { lineage },
            position_microseconds: 12_000_000,
        };
        assert_eq!(
            intent_of(set(3)),
            Some(ViewerTransportIntent::Seek(Duration::from_secs(12)))
        );
        assert_eq!(
            plan_command(&command(set(2), Some(3)), Some(&page(3)), &observation()),
            PlannedAction::Ignored(IgnoredReason::StaleTrack)
        );
    }

    #[test]
    fn commands_from_a_widget_that_saw_the_previous_video_are_ignored() {
        let stale = command(DesktopTransportAction::Pause, Some(2));
        assert_eq!(
            plan_command(&stale, Some(&page(3)), &observation()),
            PlannedAction::Ignored(IgnoredReason::StaleControl)
        );
        let no_revision = command(DesktopTransportAction::Pause, None);
        assert_eq!(
            plan_command(&no_revision, Some(&page(3)), &observation()),
            PlannedAction::Ignored(IgnoredReason::StaleControl)
        );
        assert_eq!(
            plan_command(&stale, None, &observation()),
            PlannedAction::Ignored(IgnoredReason::NoPageYet)
        );
    }

    #[test]
    fn queue_actions_do_not_exist_in_this_window_and_volume_is_local() {
        for action in [
            DesktopTransportAction::Next,
            DesktopTransportAction::Previous,
        ] {
            assert_eq!(
                plan_command(&command(action, Some(3)), Some(&page(3)), &observation()),
                PlannedAction::Ignored(IgnoredReason::NoQueueInThisWindow)
            );
        }
        let volume = EffectiveVolume::from_player(0.25).expect("volume");
        assert_eq!(
            plan_command(
                &command(DesktopTransportAction::SetVolume(volume), None),
                None,
                &observation()
            ),
            PlannedAction::Player(PlayerCommand::SetVolume(0.25))
        );
    }

    #[test]
    fn the_view_carries_title_domain_and_never_offers_the_queue() {
        let view = build_view(4, Some(&page(3)), &observation());
        assert_eq!(view.metadata.title.as_deref(), Some("Cats"));
        assert_eq!(
            view.metadata.collection_context.as_deref(),
            Some("video.example")
        );
        assert_eq!(view.control_revision, Some(DesktopControlRevision::new(3)));
        assert_eq!(view.playback_status, DesktopPlaybackStatus::Playing);
        assert!(
            view.capabilities.can_play && view.capabilities.can_pause && view.capabilities.can_seek
        );
        assert!(!view.capabilities.can_go_next && !view.capabilities.can_go_previous);
    }

    #[test]
    fn a_page_without_a_title_is_named_by_its_domain() {
        let untitled = PageInfo {
            lineage: 1,
            label: PageLabel::new("  ", "video.example"),
        };
        let view = build_view(1, Some(&untitled), &observation());
        assert_eq!(view.metadata.title.as_deref(), Some("video.example"));
        assert_eq!(view.metadata.collection_context, None);
    }

    #[test]
    fn without_a_page_the_view_is_neutral_and_offers_nothing() {
        let view = build_view(1, None, &observation());
        assert_eq!(view.playback_status, DesktopPlaybackStatus::Stopped);
        assert_eq!(view.capabilities, DesktopCapabilities::default());
        assert_eq!(view.control_revision, None);
    }

    #[test]
    fn only_a_position_change_does_not_call_for_a_full_publication() {
        let before = build_view(1, Some(&page(3)), &observation());
        let mut moved = observation();
        moved.position = MediaTime::from_secs(31);
        assert!(!differs_beyond_position(
            &before,
            &build_view(2, Some(&page(3)), &moved)
        ));
        let mut paused = observation();
        paused.playback_state = PlaybackState::Paused;
        assert!(differs_beyond_position(
            &before,
            &build_view(2, Some(&page(3)), &paused)
        ));
        assert!(differs_beyond_position(
            &before,
            &build_view(2, Some(&page(4)), &observation())
        ));
    }
}
