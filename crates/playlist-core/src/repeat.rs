//! Постоянная доменная политика повторения очереди.

/// Режим обработки конца текущего элемента и canonical boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RepeatMode {
    /// Остановить воспроизведение на конце canonical очереди.
    StopAtEnd,
    /// После конца canonical очереди продолжить с первого элемента.
    RepeatQueue,
    /// После clean `Ended` повторно проиграть текущий элемент.
    RepeatOne,
}
