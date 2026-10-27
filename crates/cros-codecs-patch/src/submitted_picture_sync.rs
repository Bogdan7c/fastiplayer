// Локальный патч Fastiplayer (gles2-renderer, подсессия 12a.1).
//
//! Ожидание ускорителя после отправки картинки — одна политика для всех stateless-декодеров.
//!
//! Раньше каждый кодек сам писал `if blocking_mode == Blocking { handle.sync() }`.
//! Режим [`BlockingMode::SyncPreviousPicture`] требует помнить прошлую отправленную
//! картинку, поэтому правило собрано у одного владельца.
//!
//! Зачем этот режим. В `NonBlocking` декодер не ждёт ускоритель вовсе. Готовность кадра
//! проверяет только тот, кто получил его через `FrameReady`, а картинки, которые кодек держит
//! у себя (перестановка B-кадров в DPB), в `FrameReady` не попадают. Поэтому после паузы
//! декодер мог залить в GPU 5–7 картинок подряд. На GPU, где декод и рисование делят одни
//! блоки (Intel Ironlake), показ кадра стоял в очереди за этой пачкой и опаздывал к vsync.
//! `SyncPreviousPicture` держит в ускорителе не больше двух картинок и сохраняет перекрытие:
//! пока GPU декодирует картинку k−1, CPU уже разобрал и отправил картинку k.

use crate::BlockingMode;

/// Отправленная в ускоритель картинка, окончания которой можно дождаться.
///
/// Отдельный трейт вместо `DecodedHandle`, чтобы политика и её тесты не зависели от фичи
/// `backend`. Для всех `DecodedHandle` он реализован в `decoder::stateless`.
#[cfg_attr(
    not(any(feature = "backend", test)),
    allow(dead_code, reason = "Используется только stateless-декодерами фичи `backend`.")
)]
pub(crate) trait AwaitablePicture: Clone {
    /// Ошибка ожидания backend-а.
    type WaitError;

    /// Блокирует поток, пока ускоритель не закончит эту картинку.
    fn wait_until_decoded(&self) -> Result<(), Self::WaitError>;
}

/// Помнит последнюю отправленную картинку, если режим требует дождаться её позже.
#[cfg_attr(
    not(any(feature = "backend", test)),
    allow(dead_code, reason = "Используется только stateless-декодерами фичи `backend`.")
)]
pub(crate) struct SubmittedPictureSync<P> {
    /// Картинка прошлой отправки, которую ещё не дождались.
    /// Бывает заполнена только в режиме `SyncPreviousPicture`.
    awaiting_completion: Option<P>,
}

impl<P> Default for SubmittedPictureSync<P> {
    fn default() -> Self {
        Self { awaiting_completion: None }
    }
}

#[cfg_attr(
    not(any(feature = "backend", test)),
    allow(dead_code, reason = "Используется только stateless-декодерами фичи `backend`.")
)]
impl<P: AwaitablePicture> SubmittedPictureSync<P> {
    /// Применяет режим ожидания сразу после того, как `submitted` ушла в ускоритель.
    ///
    /// - `Blocking`: ждём саму `submitted`.
    /// - `NonBlocking`: не ждём ничего.
    /// - `SyncPreviousPicture`: запоминаем `submitted` и ждём картинку прошлой отправки.
    ///   Ускоритель к этому моменту уже получил `submitted`, поэтому не простаивает.
    ///
    /// Ошибка ожидания возвращается как есть, тем же путём, что ошибка `Blocking`.
    /// Ждавшаяся картинка при ошибке всё равно отпускается, а `submitted` остаётся
    /// запомненной: следующий вызов дождётся уже её.
    pub(crate) fn after_picture_submitted(
        &mut self,
        mode: BlockingMode,
        submitted: &P,
    ) -> Result<(), P::WaitError> {
        match mode {
            BlockingMode::Blocking => submitted.wait_until_decoded(),
            BlockingMode::NonBlocking => Ok(()),
            BlockingMode::SyncPreviousPicture => {
                match self.awaiting_completion.replace(submitted.clone()) {
                    Some(previous) => previous.wait_until_decoded(),
                    None => Ok(()),
                }
            }
        }
    }

    /// Дожидается запомненной картинки и отпускает её.
    ///
    /// Зовётся на границе жизненного цикла (`flush`): после неё декодер не должен держать
    /// картинку старого потока до следующей отправки, иначе её поверхность дольше занята.
    pub(crate) fn complete_awaiting_picture(&mut self) -> Result<(), P::WaitError> {
        match self.awaiting_completion.take() {
            Some(previous) => previous.wait_until_decoded(),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
