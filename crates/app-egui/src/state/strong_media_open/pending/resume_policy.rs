//! Политика «resume у самого конца»: когда сохранённую позицию не восстанавливаем.
//!
//! Владелец: этот модуль — единственное место, где определяется, считать ли
//! сохранённую позицию «практически концом трека». Решение требует длительности,
//! которая известна только после установки медиа (`Installed`), поэтому политика
//! вызывается из `plan_post_installed_startup_position` — единственной точки, через
//! которую проходит любое восстановление позиции из resume-checkpoint-а
//! (старт приложения и возврат к элементу очереди идут тем же путём).
//!
//! Формат checkpoint-а и само сохранение не затрагиваются: политика работает
//! только в момент чтения.

use std::time::Duration;

/// Доля длительности (в процентах), меньше которой «до конца осталось слишком мало».
/// Решение владельца (план ux-edge-cases-2, B2): 5 %.
pub(super) const NEAR_END_PERCENT_OF_DURATION: u32 = 5;

/// Абсолютный остаток, меньше которого трек считается дослушанным.
/// Решение владельца (план ux-edge-cases-2, B2): 10 секунд.
pub(super) const NEAR_END_MIN_REMAINING: Duration = Duration::from_secs(10);

/// Нужно ли начинать трек с начала вместо восстановления `saved_position`.
///
/// Правило: остаток `duration - saved_position` строго меньше 5 % длительности
/// ИЛИ строго меньше 10 секунд. Границы не включены: ровно 5 % и ровно 10 с
/// остатка ещё восстанавливаются (в пользу пользователя: не теряем позицию без
/// явной причины). Для треков короче 10 с условие «< 10 с» выполняется всегда —
/// они честно начинаются с начала.
///
/// `duration == None` (длительность неизвестна: live или ещё не прочитана) —
/// правило не применяется и возвращается `false`: без длительности нельзя честно
/// сказать, что позиция у конца, а прежнее поведение (восстановить) безопаснее,
/// чем молча терять позицию. Позиция за пределами длительности сюда не доходит
/// (её обрабатывает ветка stale-checkpoint с предупреждением), но при вызове
/// остаток насыщается в ноль и трек тоже начался бы с начала.
pub(super) fn should_restart_from_beginning(
    saved_position: Duration,
    duration: Option<Duration>,
) -> bool {
    let Some(duration) = duration else {
        return false;
    };
    let remaining = duration.saturating_sub(saved_position);
    // Сравнение в целых наносекундах: остаток * 100 < длительность * процент —
    // без float и потерь точности.
    let below_percent_threshold =
        remaining.as_nanos() * 100 < duration.as_nanos() * u128::from(NEAR_END_PERCENT_OF_DURATION);
    below_percent_threshold || remaining < NEAR_END_MIN_REMAINING
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn secs(value: u64) -> Duration {
        Duration::from_secs(value)
    }

    #[test]
    fn three_seconds_before_end_of_ten_minute_track_restarts() {
        assert!(should_restart_from_beginning(secs(597), Some(secs(600))));
    }

    #[test]
    fn four_percent_before_end_of_one_hour_track_restarts() {
        // 4 % от 3600 с = 144 с остатка: проходит по проценту, хотя > 10 с.
        assert!(should_restart_from_beginning(
            secs(3600 - 144),
            Some(secs(3600))
        ));
    }

    #[test]
    fn six_percent_and_more_than_ten_seconds_remaining_restores() {
        // 6 % от 3600 с = 216 с.
        assert!(!should_restart_from_beginning(
            secs(3600 - 216),
            Some(secs(3600))
        ));
        assert!(!should_restart_from_beginning(secs(100), Some(secs(600))));
    }

    #[test]
    fn unknown_duration_never_restarts() {
        assert!(!should_restart_from_beginning(secs(9999), None));
    }

    #[test]
    fn exactly_five_percent_remaining_is_restored() {
        // 5 % от 1000 с = 50 с: граница не включена, восстанавливаем.
        assert!(!should_restart_from_beginning(secs(950), Some(secs(1000))));
        assert!(should_restart_from_beginning(
            secs(950) + Duration::from_millis(1),
            Some(secs(1000))
        ));
    }

    #[test]
    fn exactly_ten_seconds_remaining_is_restored() {
        // 10 с от 1000 с — это 1 % (<5 %), поэтому проверяем границу 10 с на
        // коротком треке, где процентный порог (5 % от 100 с = 5 с) её не маскирует.
        assert!(!should_restart_from_beginning(secs(90), Some(secs(100))));
        assert!(should_restart_from_beginning(
            secs(90) + Duration::from_millis(1),
            Some(secs(100))
        ));
    }

    #[test]
    fn track_shorter_than_ten_seconds_always_restarts() {
        assert!(should_restart_from_beginning(secs(1), Some(secs(8))));
        assert!(should_restart_from_beginning(Duration::ZERO, Some(secs(8))));
    }
}
