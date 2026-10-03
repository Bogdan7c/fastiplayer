//! Пользовательские пороги A/V-синхронизации действительно меняют решение:
//! одно и то же расхождение даёт разные действия при разных порогах.

use super::*;

#[test]
fn custom_thresholds_override_default_present_and_drop_windows() {
    // Узкие пороги: present-окно 5 мс, drop после 10 мс отставания.
    let strict = AvSync::new(5.0, 10.0);
    let default = AvSync::default();

    // Видео отстаёт на 20 мс: по умолчанию это ещё «показать», строго — «выбросить».
    assert_eq!(default.decide(ms(80.0), ms(100.0)), FrameAction::Present);
    assert_eq!(strict.decide(ms(80.0), ms(100.0)), FrameAction::Drop);

    // Видео отстаёт на 7 мс: строго — вне present-окна, но ещё не drop.
    assert_eq!(strict.decide(ms(93.0), ms(100.0)), FrameAction::Present);

    // Видео опережает на 8 мс: по умолчанию показываем сразу, строго — ждём.
    assert_eq!(default.decide(ms(108.0), ms(100.0)), FrameAction::Present);
    assert!(matches!(
        strict.decide(ms(108.0), ms(100.0)),
        FrameAction::Wait(wait) if wait.as_millis() == 8
    ));
}
