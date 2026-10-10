//! Слияние промежуточных действий controls перед отправкой в player worker.
//!
//! Громкость — абсолютное значение: из нескольких `SetVolume` подряд важна только
//! последняя. Отправлять каждую — значит зря расходовать очередь команд worker-а
//! (сессия UX 17). Сливаются только *соседние* `SetVolume`: если между ними стоит другое
//! действие (например, mute), порядок и смысл последовательности сохраняются.

use super::player_controls::ControlAction;

/// Схлопывает каждую серию подряд идущих `SetVolume` в последнюю из них.
#[must_use]
pub(crate) fn coalesce_consecutive_volume(actions: Vec<ControlAction>) -> Vec<ControlAction> {
    let mut coalesced: Vec<ControlAction> = Vec::with_capacity(actions.len());
    for action in actions {
        if let ControlAction::SetVolume(_) = action
            && let Some(ControlAction::SetVolume(_)) = coalesced.last()
        {
            // Предыдущее значение той же серии перекрыто более новым.
            coalesced.pop();
        }
        coalesced.push(action);
    }
    coalesced
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousand_volume_updates_in_one_frame_become_the_final_one() {
        let mut actions: Vec<ControlAction> = (0..1_000_u16)
            .map(|step| ControlAction::SetVolume(f32::from(step) / 2_000.0))
            .collect();
        actions.push(ControlAction::SetVolume(0.75));

        let coalesced = coalesce_consecutive_volume(actions);

        assert_eq!(coalesced, vec![ControlAction::SetVolume(0.75)]);
    }

    #[test]
    fn volume_series_separated_by_mute_keep_their_order() {
        let actions = vec![
            ControlAction::SetVolume(0.1),
            ControlAction::SetVolume(0.2),
            ControlAction::ToggleMute,
            ControlAction::SetVolume(0.3),
            ControlAction::SetVolume(0.4),
        ];

        let coalesced = coalesce_consecutive_volume(actions);

        assert_eq!(
            coalesced,
            vec![
                ControlAction::SetVolume(0.2),
                ControlAction::ToggleMute,
                ControlAction::SetVolume(0.4),
            ]
        );
    }

    #[test]
    fn non_volume_actions_are_never_merged() {
        let actions = vec![
            ControlAction::ToggleMute,
            ControlAction::ToggleMute,
            ControlAction::AdjustPlaybackRateSteps(1),
            ControlAction::AdjustPlaybackRateSteps(1),
        ];

        let coalesced = coalesce_consecutive_volume(actions.clone());

        assert_eq!(coalesced, actions);
    }

    #[test]
    fn empty_frame_stays_empty() {
        assert!(coalesce_consecutive_volume(Vec::new()).is_empty());
    }
}
