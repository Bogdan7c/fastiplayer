//! Поток-рисовальщик субтитров и его части (сессия 09, этап 1).
//!
//! # Архитектура
//! - `validity`: срок годности готовой картинки `[rendered_at, valid_until)` во времени субтитров.
//! - `schedule`: чистое планирование — по «часам показа» и уже готовым интервалам решает, на
//!   какое lookup-время рисовать следующим (или что делать нечего). Без потоков и растеризатора.
//! - `prepared_queue`: UI-сторона — очередь готовых overlay и выбор картинки для кадра
//!   (точная / последняя без закончившихся реплик / ничего). Растеризатор не вызывает.
//! - `commands`: язык общения UI и потока (команды и события).
//! - `worker` + `render_step` + `track_mirror` + `published_record`: сам поток — владелец
//!   растеризатора, зеркал дорожек и памяти о выданном.
//! - `handle`: UI-сторона потока (создание, отправка, приём, остановка).
//! - `mirror_sync`, `clock_tracker`, `lateness`: UI-сторона — как держать зеркала в актуальном
//!   состоянии, как выводить «часы показа», как считать опоздания.
//!
//! # Границы
//! UI-поток никогда не растеризует и не ждёт рисовальщика; фоллбэк никогда не берёт картинку
//! другого источника (`CueSourceId`). Отказ потока (не запустился, упал) — типизированная
//! [`RenderWorkerFailure`], а не паника: субтитры становятся недоступны, видео играет.

pub(super) mod clock_tracker;
pub(super) mod commands;
pub(super) mod handle;
pub(super) mod lateness;
pub(super) mod mirror_sync;
pub(super) mod prepared_queue;
mod published_record;
mod render_step;
pub(super) mod schedule;
mod track_mirror;
pub(super) mod validity;
pub(super) mod worker;

use std::io;

/// Почему поток-рисовальщик недоступен.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RenderWorkerFailure {
    /// Поток не удалось создать.
    StartFailed {
        /// Вид ошибки ввода-вывода при создании потока.
        kind: io::ErrorKind,
    },
    /// Поток завершился сам (паника или потерянный канал).
    Lost,
}
