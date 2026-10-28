//! Такт декодера по проходам показа рендера (gles2-renderer 12a.2, решения владельца 77–78).
//!
//! Корень проблемы. На Intel Ironlake аппаратный H.264 и проход показа GLES2 стоят в одной
//! FIFO-очереди GPU без вытеснения. На HW 1080p30 декодер работает почти без пауз (картинка —
//! ~27 мс GPU при бюджете 33 мс), поэтому в очереди почти всегда лежит его картинка, и наш
//! проход показа ждёт за ней. Кадр показывается на vsync позже (замер T410: 34–40 опозданий
//! за 20 с).
//!
//! Правило. Пока у декодера есть запас готовых кадров, следующий пакет (а с ним — отправка
//! картинки на GPU, cros-codecs H.264 отправляет картинку k при подаче AU k+1) подаётся только
//! после сигнала рендера «проход показа отправлен». Одна подача на один показ: в очереди GPU
//! получается «показ → декод», и следующий показ приходит к свободному GPU. Временная сборка
//! на T410: 0 опозданий за 20 с на 1080p30, CPU не выше.
//!
//! Чего правило НЕ делает:
//! - без подписки (рендер wgpu не даёт сигнала) поведение прежнее — ни одного ожидания;
//! - при малом запасе (старт, seek, декодер отстаёт) декодер не ждёт: догоняет как раньше;
//! - control message (flush/seek/configure/release/EOF drain) прерывает ожидание сразу;
//! - пакет из канала не вынимается, пока ждём: flush сбрасывает его обычным путём;
//! - если сигнала нет дольше [`PRESENT_PULSE_WAIT_LIMIT`] (окно скрыто, рендер ничего не
//!   показывает), пакет подаётся без него — декод не может застрять из-за рендера.

use std::time::Duration;

use crossbeam_channel::{Receiver, TryRecvError, select_biased};
use video_core::PresentPassSubscription;

use super::control::ThreadControlMsg;

/// С какого запаса декодер начинает ждать сигнала показа.
///
/// Запас — кадры, которые декодер уже выдал и ещё не получил обратно (у плеера, в очереди
/// показа, у рендера до fence), плюс кадры «в полёте» на GPU. Рендер сам держит ~2 кадра
/// (текущий и прошлый до fence), так что 4 — это «впереди показа есть ещё хотя бы два
/// готовых кадра». На T410 в установившемся режиме запас 10–12 кадров; ниже порога
/// декодер работает без такта и догоняет. Значение проверено экспериментом 12a.2.
pub(super) const PRESENT_PACING_MIN_DECODED_FRAMES_AHEAD: usize = 4;

/// Сколько декодер ждёт сигнала показа, прежде чем подать пакет без него.
///
/// Больше интервала кадра самого медленного обычного видео (24 fps — 41,7 мс), чтобы на
/// живом показе предел не срабатывал; при остановленном показе декод идёт хотя бы 10
/// пакетов/с, а запас при этом тает и такт выключается сам.
pub(super) const PRESENT_PULSE_WAIT_LIMIT: Duration = Duration::from_millis(100);

/// Сколько декодированных кадров декодер держит впереди показа.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DecodedFramesAhead(pub(crate) usize);

/// Решение «можно ли сейчас вынуть следующий пакет».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PacketAdmission {
    /// Пакет подаётся сразу.
    Admit,
    /// Сначала дождаться сигнала показа (или предела ожидания) — см. `wait_for_present_pulse_or_control`.
    AwaitPresentPulse,
}

/// Откуда взялось разрешение подать пакет после ожидания.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PacingPermitSource {
    /// Рендер отправил проход показа.
    PresentPulse,
    /// Сигнала не было дольше предела ожидания.
    WaitLimitReached,
    /// Источник сигнала уничтожен (рендер пересоздан/закрыт): дальше без такта.
    SignalGone,
}

/// Чем закончилось ожидание сигнала показа.
pub(super) enum PresentPulseWake {
    /// Можно подавать следующий пакет.
    PermitGranted(PacingPermitSource),
    /// Пришла control message; поток обязан обработать её сразу.
    Control(ThreadControlMsg),
    /// Все control sender-ы закрыты — decoder loop завершается.
    ControlDisconnected,
}

/// Счётчики такта для диагностики (пишутся в лог при штатном завершении потока).
///
/// Считаются решения шага цикла, а не пакеты: шаг без пакета в канале тоже решает,
/// можно ли вынимать следующий.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct PresentPacingCounters {
    /// Решения «без такта» из-за малого запаса.
    pub(super) low_lead_admits: u64,
    /// Решения «можно», когда сигнал показа уже лежал к моменту решения.
    pub(super) ready_pulse_admits: u64,
    /// Ожидания, завершённые сигналом показа.
    pub(super) present_pulse_waits: u64,
    /// Ожидания, завершённые пределом времени (показа не было).
    pub(super) wait_limit_reached: u64,
}

/// Состояние такта одного decoder thread-а.
pub(super) struct PresentPacing {
    mode: PacingMode,
    counters: PresentPacingCounters,
}

/// Работает ли такт вообще.
enum PacingMode {
    /// Рендер не даёт сигнала (wgpu) или источник уничтожен: декодер не ждёт никогда.
    Unpaced,
    /// Подписка на сигнал показа и разрешение на следующую подачу.
    Paced {
        subscription: PresentPassSubscription,
        permit: SubmitPermit,
    },
}

/// Есть ли разрешение на подачу следующего пакета.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubmitPermit {
    /// Показ был после прошлой подачи (или запас мал) — подавать можно.
    Granted,
    /// Разрешение потрачено прошлой подачей — ждать нового показа.
    Spent,
}

impl PresentPacing {
    /// Такт выключен: прежнее поведение декодера.
    pub(super) fn unpaced() -> Self {
        Self {
            mode: PacingMode::Unpaced,
            counters: PresentPacingCounters::default(),
        }
    }

    /// Такт по сигналу показа. Первая подача разрешена сразу.
    pub(super) fn paced(subscription: PresentPassSubscription) -> Self {
        Self {
            mode: PacingMode::Paced {
                subscription,
                permit: SubmitPermit::Granted,
            },
            counters: PresentPacingCounters::default(),
        }
    }

    /// Решает, можно ли вынуть следующий пакет прямо сейчас. Никогда не блокирует.
    pub(super) fn admission_for_next_packet(
        &mut self,
        frames_ahead: DecodedFramesAhead,
    ) -> PacketAdmission {
        let PacingMode::Paced {
            subscription,
            permit,
        } = &mut self.mode
        else {
            return PacketAdmission::Admit;
        };
        if frames_ahead < DecodedFramesAhead(PRESENT_PACING_MIN_DECODED_FRAMES_AHEAD) {
            // Запаса нет — догоняем без такта. Старый сигнал выбрасываем: иначе, набрав запас,
            // декодер подал бы пакет по давнему показу, то есть в случайной фазе.
            let pulse = subscription.pulse_receiver().try_recv();
            *permit = SubmitPermit::Granted;
            self.counters.low_lead_admits += 1;
            if pulse == Err(TryRecvError::Disconnected) {
                self.mode = PacingMode::Unpaced;
            }
            return PacketAdmission::Admit;
        }
        if *permit == SubmitPermit::Granted {
            return PacketAdmission::Admit;
        }
        match subscription.pulse_receiver().try_recv() {
            Ok(()) => {
                // Показ уже был, пока декодер подавал прошлый пакет: подаём сразу после него.
                *permit = SubmitPermit::Granted;
                self.counters.ready_pulse_admits += 1;
                PacketAdmission::Admit
            }
            Err(TryRecvError::Empty) => PacketAdmission::AwaitPresentPulse,
            Err(TryRecvError::Disconnected) => {
                self.mode = PacingMode::Unpaced;
                PacketAdmission::Admit
            }
        }
    }

    /// Ждёт сигнал показа, control message или предел `wait_limit`.
    ///
    /// Control message имеет приоритет: flush/seek не ждут показа. После control message
    /// разрешение не выдаётся — следующий шаг цикла решит заново (flush мог сбросить пакеты).
    pub(super) fn wait_for_present_pulse_or_control(
        &mut self,
        control_rx: &Receiver<ThreadControlMsg>,
        wait_limit: Duration,
    ) -> PresentPulseWake {
        let PacingMode::Paced {
            subscription,
            permit,
        } = &mut self.mode
        else {
            return PresentPulseWake::PermitGranted(PacingPermitSource::SignalGone);
        };
        let source = select_biased! {
            recv(control_rx) -> control_result => {
                return match control_result {
                    Ok(control_message) => PresentPulseWake::Control(control_message),
                    Err(_) => PresentPulseWake::ControlDisconnected,
                };
            },
            recv(subscription.pulse_receiver()) -> pulse_result => match pulse_result {
                Ok(()) => PacingPermitSource::PresentPulse,
                Err(_) => PacingPermitSource::SignalGone,
            },
            default(wait_limit) => PacingPermitSource::WaitLimitReached,
        };
        *permit = SubmitPermit::Granted;
        match source {
            PacingPermitSource::PresentPulse => self.counters.present_pulse_waits += 1,
            PacingPermitSource::WaitLimitReached => self.counters.wait_limit_reached += 1,
            PacingPermitSource::SignalGone => self.mode = PacingMode::Unpaced,
        }
        PresentPulseWake::PermitGranted(source)
    }

    /// Пакет вынут из канала и подан декодеру: разрешение потрачено.
    pub(super) fn note_packet_submitted(&mut self) {
        if let PacingMode::Paced { permit, .. } = &mut self.mode {
            *permit = SubmitPermit::Spent;
        }
    }

    /// Работает ли такт (для логов и тестов).
    pub(super) fn is_paced(&self) -> bool {
        matches!(self.mode, PacingMode::Paced { .. })
    }

    /// Снимок счётчиков такта.
    pub(super) fn counters(&self) -> PresentPacingCounters {
        self.counters
    }
}

/// Итог шага такта в цикле decoder thread-а.
pub(super) enum PacingStep {
    /// Следующий пакет можно вынимать из канала.
    Proceed,
    /// Во время ожидания пришла control message: цикл обрабатывает её и начинает шаг заново.
    Control(ThreadControlMsg),
    /// Control-канал закрыт: decoder loop завершается.
    ControlDisconnected,
    /// Запас кадров не узнать (отравлен пул ресурсов): фатальная ошибка декодера.
    Fatal(anyhow::Error),
}

/// Шаг такта перед `select!` по пакетам: при запасе кадров ждёт сигнала показа.
///
/// Пакет в это время остаётся в канале — flush/seek сбрасывают его обычным путём.
pub(super) fn await_present_pass_permit(
    decoder: &crate::VaapiVideoDecoder,
    present_pacing: &mut PresentPacing,
    control_rx: &Receiver<ThreadControlMsg>,
) -> PacingStep {
    if !present_pacing.is_paced() {
        return PacingStep::Proceed;
    }
    let frames_ahead = match decoder.decoded_frames_ahead() {
        Ok(frames_ahead) => frames_ahead,
        Err(error) => return PacingStep::Fatal(error),
    };
    if present_pacing.admission_for_next_packet(frames_ahead) == PacketAdmission::Admit {
        return PacingStep::Proceed;
    }
    match present_pacing.wait_for_present_pulse_or_control(control_rx, PRESENT_PULSE_WAIT_LIMIT) {
        PresentPulseWake::PermitGranted(source) => {
            tracing::trace!(
                ?source,
                frames_ahead = frames_ahead.0,
                "Present pacing permit"
            );
            PacingStep::Proceed
        }
        PresentPulseWake::Control(control_message) => PacingStep::Control(control_message),
        PresentPulseWake::ControlDisconnected => PacingStep::ControlDisconnected,
    }
}

/// Пишет итог такта при завершении decoder thread-а (только если такт был включён).
pub(super) fn log_present_pacing_summary(present_pacing: &PresentPacing) {
    let counters = present_pacing.counters();
    if counters == PresentPacingCounters::default() {
        return;
    }
    tracing::debug!(
        low_lead_admits = counters.low_lead_admits,
        ready_pulse_admits = counters.ready_pulse_admits,
        present_pulse_waits = counters.present_pulse_waits,
        wait_limit_reached = counters.wait_limit_reached,
        "Present pacing summary"
    );
}
