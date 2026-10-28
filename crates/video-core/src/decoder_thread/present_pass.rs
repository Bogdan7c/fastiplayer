//! Нейтральный сигнал «рендер только что отправил проход показа на GPU».
//!
//! Зачем (gles2-renderer 12a.2, решения владельца 77–78). На слабых GPU (Intel Ironlake)
//! аппаратный декод и проход показа стоят в одной FIFO-очереди GPU без вытеснения.
//! Если картинка декода (1080p — ~27 мс работы GPU) попала в очередь прямо перед нашим
//! проходом, кадр показывается на vsync позже. Декодер, который подаёт следующую картинку
//! сразу ПОСЛЕ прохода показа, получает в очереди порядок «показ → декод».
//!
//! Граница:
//! - рендер владеет [`PresentPassSignal`] и зовёт [`PresentPassSignal::notify_present_pass_submitted`]
//!   после каждой отправки прохода показа. Он не знает, кто подписан и зачем;
//! - декодер получает свою [`PresentPassSubscription`] и сам решает, ждать ли сигнала.
//!   Он не знает, какой рендер и какой графический API звонит;
//! - склеивает их composition root (app-egui): рендер без сигнала (wgpu) — декодер без такта.
//!
//! Инварианты:
//! - звонок никогда не блокирует рендер: у каждой подписки канал ёмкости 1, повторные
//!   сигналы сливаются (подписчику важен факт «показ был», а не число показов);
//! - каждая подписка получает свой жетон: два декодера (старый и кандидат нового файла)
//!   не отбирают сигнал друг у друга;
//! - уничтоженная подписка убирается из списка при следующем звонке; уничтоженный источник
//!   закрывает канал, и подписчик видит `Disconnected`, а не вечное ожидание.

use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};

/// Источник сигнала «проход показа отправлен на GPU». Им владеет рендер.
///
/// Клон указывает на тот же список подписчиков: так рендер отдаёт источник приложению,
/// а приложение — фабрике декодера, не создавая второй сигнал.
#[derive(Clone, Debug, Default)]
pub struct PresentPassSignal {
    /// Отправители жетонов живых подписок. Mutex держится только на время
    /// `retain`/`push` и не охватывает вызовов, которые могут паниковать.
    subscribers: Arc<Mutex<Vec<Sender<()>>>>,
}

/// Чем закончился звонок рендера.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentPassNotifyOutcome {
    /// Жетон положен (или уже лежал) у `subscribers` живых подписок.
    Delivered { subscribers: usize },
    /// Подписок нет (например, играет программный декодер): звонок — no-op.
    NoSubscribers,
    /// Список подписчиков отравлен паникой другого потока. Сигнал больше не работает,
    /// подписчики дождутся своего предела ожидания; рендер обязан сообщить об этом в лог.
    SignalPoisoned,
}

/// Подписаться не удалось: список подписчиков отравлен паникой другого потока.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentPassSignalPoisoned;

impl std::fmt::Display for PresentPassSignalPoisoned {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("present pass signal is poisoned by a panic in another thread")
    }
}

impl std::error::Error for PresentPassSignalPoisoned {}

impl PresentPassSignal {
    /// Создаёт источник без подписчиков.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Регистрирует нового подписчика со своим каналом жетонов ёмкости 1.
    pub fn subscribe(&self) -> Result<PresentPassSubscription, PresentPassSignalPoisoned> {
        // Ёмкость 1: подписчику важен факт «показ был после моей последней отправки».
        let (pulse_tx, pulse_rx) = bounded::<()>(1);
        let mut subscribers = self
            .subscribers
            .lock()
            .map_err(|_| PresentPassSignalPoisoned)?;
        subscribers.push(pulse_tx);
        Ok(PresentPassSubscription { pulse_rx })
    }

    /// Сообщает всем подписчикам, что проход показа только что отправлен на GPU.
    /// Никогда не блокирует: полный канал значит «жетон уже лежит».
    pub fn notify_present_pass_submitted(&self) -> PresentPassNotifyOutcome {
        let Ok(mut subscribers) = self.subscribers.lock() else {
            return PresentPassNotifyOutcome::SignalPoisoned;
        };
        // Заодно выбрасываем отправителей уничтоженных подписок (декодер прошлого файла).
        subscribers.retain(|pulse_tx| match pulse_tx.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => true,
            Err(TrySendError::Disconnected(())) => false,
        });
        match subscribers.len() {
            0 => PresentPassNotifyOutcome::NoSubscribers,
            count => PresentPassNotifyOutcome::Delivered { subscribers: count },
        }
    }
}

/// Подписка декодера на сигнал показа: один жетон на «был показ с прошлого раза».
#[derive(Debug)]
pub struct PresentPassSubscription {
    /// Канал жетонов ёмкости 1; закрывается вместе с источником.
    pulse_rx: Receiver<()>,
}

impl PresentPassSubscription {
    /// Receiver жетонов для `select!` декодера (ожидание вместе с control-каналом).
    #[must_use]
    pub fn pulse_receiver(&self) -> &Receiver<()> {
        &self.pulse_rx
    }
}

#[cfg(test)]
mod tests;
