//! Тестовое действие зрителя с субтитрами (только сквозные тесты 14a, `subtitle_test_trigger`):
//! тот же путь, что клавиши C и V.

use super::super::subtitle_test_trigger::SubtitleTestTrigger;
use super::BrowserWindowInstance;

impl BrowserWindowInstance {
    /// Проход окна: тестовая переменная, если задана, делает действие зрителя один раз.
    pub(in crate::browser_session::window) fn poll_subtitle_test_trigger(
        &mut self,
        trigger: &mut SubtitleTestTrigger,
    ) {
        trigger.poll(&mut self.app_state, &self.window);
    }
}
