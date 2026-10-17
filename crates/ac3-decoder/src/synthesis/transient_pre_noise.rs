//! Подавление пред-шума транзиентов (TPNP) во временной области
//! (A/52:2012 Annex E §3.7, Рисунок E3.2; ETSI TS 102 366 v1.4.1 §E.2.7).
//!
//! Кодер передаёт для fbw канала положение транзиента `4·transprocloc`
//! (от первого отсчёта кадра, может лежать в следующих кадрах) и длину
//! растяжения `translen = transproclen`. Декодер заменяет пред-шум перед
//! транзиентом «растянутым» звуком, который был раньше:
//!
//! - отсчёт отсчитывается от первого отсчёта кадра КОДЕРА: блок 0 кадра
//!   покрывает окном вход кодера `[−256, 256)`, поэтому начало кадра кодера
//!   — отсчёт 256 нашего выхода кадра (та же задержка 256, что ffmpeg
//!   отдаёт контейнеру). В координатах выхода `transloc = 4·transprocloc +
//!   256`. Подтверждено сверкой с декодером Nero (единственный доступный
//!   эталон с TPNP; ffmpeg n9.0.2 TPNP не применяет): на 7 событиях потока
//!   Dolby остаток против Nero в исправленных областях −23 дБ при +256
//!   (уровень остатка вне областей) и не лучше −6 дБ при любом другом
//!   сдвиге/выборе PN (перебор с шагом 4 отсчёта: `tools/tpnp_reference_fit.py`);
//! - `aud_blk_samp_loc` — передний край блока, ПРЕДШЕСТВУЮЩЕГО блоку с
//!   транзиентом (§3.7.2: «leading edge of the audio coding block prior to
//!   the block containing the transient indicates the start of the transient
//!   pre-noise»): `256·(⌊transloc/256⌋ − 1)`; окно этого блока — первое, чей
//!   шум квантования попадает до транзиента. Отсюда `PN = transloc −
//!   aud_blk_samp_loc = 256 + transloc mod 256` (256…511);
//! - буфер синтеза — `2·TC1 + PN` исходных отсчётов, начиная за
//!   `2·TC1 + 2·PN` до транзиента (`2*tc` в псевдокоде — опечатка `2*TC1`);
//! - исправляется `tot_corr_len = PN + translen + TC1` отсчётов перед
//!   транзиентом: TC1 отсчётов перекрёстного перехода «исходный → буфер»,
//!   затем буфер, последние TC2 — переход «буфер → исходный».
//!
//! Отсчёт `i` исправленной области берёт буфер в точке `i − shift`, где
//! `shift = TC1 + PN − translen ≥ 257`: исправление причинно и не требует
//! задержки выхода. Поэтому событие, которое ссылается на будущие кадры
//! («this does not create a requirement for multi-frame decoding»),
//! хранится и доделывается в следующих кадрах по уже выданным исходным
//! отсчётам (их хранит история длиной `max shift`).
//!
//! Решения там, где спецификация оставляет выбор:
//! - окна перехода — пары Ханна `sin²/cos²` (сумма = 1; §3.7.2: «standard
//!   Hanning windows have been shown to provide good results»);
//! - буфер синтеза строится из ИСХОДНЫХ (до TPNP) отсчётов — как в
//!   псевдокоде, где `synth_buf` копируется до перезаписи;
//! - событие, исправленная область которого начинается раньше первого ещё
//!   не выданного отсчёта (уже ушедший кадр), не применяется целиком:
//!   частичная замена без начального перехода дала бы разрыв сигнала.
//!
//! Владелец состояния — [`TransientPreNoiseProcessor`]: история исходных
//! отсчётов и отложенные события по выходным каналам раскладки. Смена
//! раскладки, ошибка кадра и `reset` — разрыв потока: всё забывается.

use crate::output::ChannelPosition;
use crate::syntax::eac3::audfrm::TransientPreNoise;
use crate::syntax::syncframe::SAMPLES_PER_AUDIO_BLOCK;

/// TC1: длина первого перехода и «запас» буфера синтеза (§3.7.2).
const LEADING_CROSSFADE: usize = 256;
/// TC2: длина последнего перехода перед транзиентом (§3.7.2).
const TRAILING_CROSSFADE: usize = 128;
/// Наибольший `PN`: `256 + 255` при `transloc mod 256 = 255` (фактически
/// кратен 4, но граница не зависит от этого).
const MAX_PRE_NOISE: usize = 2 * SAMPLES_PER_AUDIO_BLOCK - 1;
/// История исходных отсчётов: наибольший `shift = TC1 + PN − 0`.
const HISTORY_SAMPLES: usize = LEADING_CROSSFADE + MAX_PRE_NOISE;
/// Начало кадра кодера в координатах выхода кадра (см. описание модуля).
const ENCODER_FRAME_OFFSET: usize = SAMPLES_PER_AUDIO_BLOCK;
/// Событие ссылается не дальше `4·1023 + 256` отсчётов вперёд, кадр — не короче
/// 256 отсчётов: одновременно у канала не больше 17 событий. Предел с
/// запасом — защита памяти, а не правило потока.
const MAX_SCHEDULED_PER_CHANNEL: usize = 32;

/// Применять ли TPNP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TransientPreNoiseProcessing {
    /// Применять параметры потока (§3.7: «the reference decoder shall
    /// implement»). Значение по умолчанию.
    #[default]
    Apply,
    /// Не применять: нужен сверке с декодерами без TPNP и диагностике.
    Skip,
}

/// Одно исправление в координатах текущего кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScheduledCorrection {
    /// Положение транзиента от первого отсчёта текущего кадра.
    transient: isize,
    /// `PN`.
    pre_noise: usize,
    /// `translen`.
    scaling_length: usize,
}

impl ScheduledCorrection {
    fn from_parameters(parameters: TransientPreNoise) -> Self {
        let transient = usize::from(parameters.location_samples) + ENCODER_FRAME_OFFSET;
        Self {
            transient: transient as isize,
            pre_noise: SAMPLES_PER_AUDIO_BLOCK + transient % SAMPLES_PER_AUDIO_BLOCK,
            scaling_length: usize::from(parameters.length_samples),
        }
    }

    /// `tot_corr_len`.
    const fn corrected_length(&self) -> usize {
        self.pre_noise + self.scaling_length + LEADING_CROSSFADE
    }

    /// Первый исправляемый отсчёт (`start_samp`).
    const fn first_corrected(&self) -> isize {
        self.transient - self.corrected_length() as isize
    }

    /// Насколько раньше исправляемого отсчёта лежит его отсчёт буфера синтеза.
    const fn shift(&self) -> usize {
        LEADING_CROSSFADE + self.pre_noise - self.scaling_length
    }
}

/// Состояние одного выходного канала.
#[derive(Debug, Clone)]
struct ChannelState {
    /// Исходные (до TPNP) отсчёты, предшествующие текущему кадру; последний
    /// элемент — отсчёт прямо перед кадром.
    history: [f32; HISTORY_SAMPLES],
    scheduled: Vec<ScheduledCorrection>,
}

impl ChannelState {
    fn new() -> Self {
        Self {
            history: [0.0; HISTORY_SAMPLES],
            scheduled: Vec::new(),
        }
    }

    /// Дописывает исходные отсчёты кадра в конец истории.
    fn remember(&mut self, originals: &[f32]) {
        if originals.len() >= HISTORY_SAMPLES {
            let tail = originals.len() - HISTORY_SAMPLES;
            self.history.copy_from_slice(&originals[tail..]);
        } else {
            self.history.copy_within(originals.len().., 0);
            let start = HISTORY_SAMPLES - originals.len();
            self.history[start..].copy_from_slice(originals);
        }
    }
}

/// Раскладка, к которой относятся история и события.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LayoutKey {
    sample_rate_hz: u32,
    positions: Vec<ChannelPosition>,
}

/// Окна переходов: `fade_in[n] = sin²(π/2·(n + ½)/len)`, `fade_out = 1 − fade_in`.
#[derive(Debug, Clone)]
struct CrossfadeWindows {
    leading: [f32; LEADING_CROSSFADE],
    trailing: [f32; TRAILING_CROSSFADE],
}

fn fade_in<const LENGTH: usize>() -> [f32; LENGTH] {
    std::array::from_fn(|index| {
        let phase = std::f64::consts::FRAC_PI_2 * (index as f64 + 0.5) / LENGTH as f64;
        (phase.sin() * phase.sin()) as f32
    })
}

/// Применённые и отброшенные события (диагностика тестов и dev-сверки).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TransientPreNoiseCounters {
    /// События, принятые к исполнению.
    pub(crate) scheduled: usize,
    /// События, исправленная область которых начиналась в уже выданных отсчётах.
    pub(crate) rejected: usize,
}

/// Исполнитель TPNP по выходным каналам программы.
#[derive(Debug, Clone)]
pub(crate) struct TransientPreNoiseProcessor {
    layout: Option<LayoutKey>,
    channels: Vec<ChannelState>,
    windows: CrossfadeWindows,
    /// Копия исходных отсчётов канала текущего кадра (рабочий буфер).
    originals: Vec<f32>,
    counters: TransientPreNoiseCounters,
}

/// Кадр PCM одной раскладки (планарно), который обрабатывает TPNP.
pub(crate) struct PlanarFrame<'a> {
    pub(crate) sample_rate_hz: u32,
    pub(crate) positions: &'a [ChannelPosition],
    pub(crate) samples_per_channel: usize,
    pub(crate) planar: &'a mut [f32],
}

impl TransientPreNoiseProcessor {
    pub(crate) fn new() -> Self {
        Self {
            layout: None,
            channels: Vec::new(),
            windows: CrossfadeWindows {
                leading: fade_in(),
                trailing: fade_in(),
            },
            originals: Vec::new(),
            counters: TransientPreNoiseCounters::default(),
        }
    }

    /// Забывает историю и события (разрыв потока).
    pub(crate) fn discard(&mut self) {
        self.layout = None;
        self.channels.clear();
    }

    #[cfg(test)]
    pub(crate) const fn counters(&self) -> TransientPreNoiseCounters {
        self.counters
    }

    /// Обрабатывает кадр: принимает новые события кадра (`events[канал]` —
    /// параметры TPNP источника этого выходного канала) и исправляет PCM.
    pub(crate) fn process_frame(
        &mut self,
        frame: PlanarFrame<'_>,
        events: &[Option<TransientPreNoise>],
    ) {
        self.begin_frame(frame.sample_rate_hz, frame.positions);
        let length = frame.samples_per_channel;
        for (channel_index, (state, pcm)) in self
            .channels
            .iter_mut()
            .zip(frame.planar.chunks_exact_mut(length.max(1)))
            .enumerate()
        {
            if let Some(parameters) = events.get(channel_index).copied().flatten() {
                let correction = ScheduledCorrection::from_parameters(parameters);
                if correction.first_corrected() < 0 {
                    self.counters.rejected += 1;
                } else if state.scheduled.len() < MAX_SCHEDULED_PER_CHANNEL {
                    self.counters.scheduled += 1;
                    state.scheduled.push(correction);
                }
            }
            if state.scheduled.is_empty() {
                state.remember(pcm);
                continue;
            }
            self.originals.clear();
            self.originals.extend_from_slice(pcm);
            for correction in &state.scheduled {
                apply_correction(
                    correction,
                    &self.windows,
                    SourceSamples {
                        history: &state.history,
                        frame: &self.originals,
                    },
                    pcm,
                );
            }
            // Событие с транзиентом дальше конца кадра доделывается в следующем.
            state.scheduled.retain_mut(|correction| {
                correction.transient -= length as isize;
                correction.transient > 0
            });
            state.remember(&self.originals);
        }
    }

    fn begin_frame(&mut self, sample_rate_hz: u32, positions: &[ChannelPosition]) {
        let unchanged = self.layout.as_ref().is_some_and(|layout| {
            layout.sample_rate_hz == sample_rate_hz && layout.positions == positions
        });
        if !unchanged {
            self.layout = Some(LayoutKey {
                sample_rate_hz,
                positions: positions.to_vec(),
            });
            self.channels = vec![ChannelState::new(); positions.len()];
        }
    }
}

/// Исходные отсчёты канала: история до кадра и сам кадр.
#[derive(Clone, Copy)]
struct SourceSamples<'a> {
    history: &'a [f32; HISTORY_SAMPLES],
    frame: &'a [f32],
}

impl SourceSamples<'_> {
    /// Исходный отсчёт с номером `index` от начала кадра (отрицательный — история).
    fn at(&self, index: isize) -> f32 {
        if index >= 0 {
            self.frame.get(index as usize).copied().unwrap_or(0.0)
        } else {
            let back = index.unsigned_abs();
            HISTORY_SAMPLES
                .checked_sub(back)
                .and_then(|position| self.history.get(position))
                .copied()
                .unwrap_or(0.0)
        }
    }
}

/// Исправляет часть области события, попавшую в текущий кадр (псевдокод §3.7.2).
fn apply_correction(
    correction: &ScheduledCorrection,
    windows: &CrossfadeWindows,
    source: SourceSamples<'_>,
    pcm: &mut [f32],
) {
    let start = correction.first_corrected();
    let total = correction.corrected_length();
    let trailing_start = total - TRAILING_CROSSFADE;
    let shift = correction.shift() as isize;
    let first = start.max(0) as usize;
    let end = (correction.transient.max(0) as usize).min(pcm.len());
    for (index, sample) in pcm.iter_mut().enumerate().take(end).skip(first) {
        let position = index as isize;
        let offset = (position - start) as usize;
        let original = source.at(position);
        let synthesis = source.at(position - shift);
        *sample = if offset < LEADING_CROSSFADE {
            let fade = windows.leading[offset];
            original * (1.0 - fade) + synthesis * fade
        } else if offset < trailing_start {
            synthesis
        } else {
            let fade = windows.trailing[offset - trailing_start];
            original * fade + synthesis * (1.0 - fade)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HISTORY_SAMPLES, LEADING_CROSSFADE, PlanarFrame, ScheduledCorrection, TRAILING_CROSSFADE,
        TransientPreNoiseProcessor,
    };
    use crate::output::ChannelPosition::{Center, Left, Right};
    use crate::syntax::eac3::audfrm::TransientPreNoise;

    const FRAME: usize = 1536;

    fn event(location_samples: u16, length_samples: u8) -> TransientPreNoise {
        TransientPreNoise {
            location_samples,
            length_samples,
        }
    }

    /// Прямая (не потоковая) реализация псевдокода §3.7.2 над всем сигналом.
    fn reference(signal: &[f32], transient: usize, translen: usize) -> Vec<f32> {
        let pn = 256 + transient % 256;
        // В координатах выхода: transloc + 256 (см. ENCODER_FRAME_OFFSET).
        let tot = pn + translen + LEADING_CROSSFADE;
        let synth: Vec<f32> = (0..2 * LEADING_CROSSFADE + pn)
            .map(|samp| signal[transient - (2 * LEADING_CROSSFADE + 2 * pn) + samp])
            .collect();
        let mut out = signal.to_vec();
        let start = transient - tot;
        let fade = |n: usize, len: usize| {
            let phase = std::f64::consts::FRAC_PI_2 * (n as f64 + 0.5) / len as f64;
            (phase.sin() * phase.sin()) as f32
        };
        for samp in 0..LEADING_CROSSFADE {
            let f = fade(samp, LEADING_CROSSFADE);
            out[start + samp] = signal[start + samp] * (1.0 - f) + synth[samp] * f;
        }
        let middle = LEADING_CROSSFADE..tot - TRAILING_CROSSFADE;
        out[start + middle.start..start + middle.end].copy_from_slice(&synth[middle]);
        for samp in tot - TRAILING_CROSSFADE..tot {
            let f = fade(samp - (tot - TRAILING_CROSSFADE), TRAILING_CROSSFADE);
            out[start + samp] = signal[start + samp] * f + synth[samp] * (1.0 - f);
        }
        out
    }

    fn ramp(length: usize) -> Vec<f32> {
        (0..length)
            .map(|index| ((index as f32) * 0.013).sin() + index as f32 * 1e-4)
            .collect()
    }

    /// Потоковое исполнение кадрами `frame_length` со своим событием в кадре
    /// `event_frame`.
    fn stream(
        signal: &[f32],
        frame_length: usize,
        event_frame: usize,
        parameters: TransientPreNoise,
    ) -> (Vec<f32>, TransientPreNoiseProcessor) {
        let mut processor = TransientPreNoiseProcessor::new();
        let mut output = Vec::new();
        for (index, chunk) in signal.chunks(frame_length).enumerate() {
            let mut pcm = chunk.to_vec();
            let events = [(index == event_frame).then_some(parameters)];
            processor.process_frame(
                PlanarFrame {
                    sample_rate_hz: 48_000,
                    positions: &[Center],
                    samples_per_channel: pcm.len(),
                    planar: &mut pcm,
                },
                &events,
            );
            output.extend(pcm);
        }
        (output, processor)
    }

    #[test]
    fn streaming_matches_pseudo_code_inside_one_frame() {
        let signal = ramp(4 * FRAME);
        // Транзиент в кадре 2: transprocloc·4 = 1000, в выходе — 1000 + 256.
        let (output, processor) = stream(&signal, FRAME, 2, event(1000, 100));
        let transient = 2 * FRAME + 1000 + 256;
        let expected = reference(&signal, transient, 100);
        assert_eq!(output, expected);
        assert_eq!(processor.counters().scheduled, 1);
        // Вне области (PN = 256 + 232) ничего не изменилось.
        let first = transient - (488 + 100 + 256);
        assert!(output[..first] == signal[..first]);
        assert!(output[transient..] == signal[transient..]);
        assert_ne!(output[first + 300], signal[first + 300]);
    }

    /// Транзиент через два кадра однoблочного потока (256 отсчётов) и
    /// источник в истории: событие доделывается в следующих кадрах.
    #[test]
    fn events_spanning_later_frames_finish_there() {
        let signal = ramp(40 * 256);
        let (output, _) = stream(&signal, 256, 20, event(4 * 900, 255));
        assert_eq!(output, reference(&signal, 20 * 256 + 3600 + 256, 255));
        let (output, _) = stream(&signal, FRAME, 1, event(4 * 1023, 0));
        assert_eq!(output, reference(&signal, FRAME + 4092 + 256, 0));
    }

    #[test]
    fn events_reaching_into_emitted_samples_are_rejected() {
        let signal = ramp(3 * FRAME);
        // transloc 400 + 256 = 656: область начинается за 256+144+255 = 655
        // + 256 (TC1) отсчётов до него — в уже выданном кадре.
        let (output, processor) = stream(&signal, FRAME, 1, event(400, 255));
        assert_eq!(output, signal);
        assert_eq!(processor.counters().rejected, 1);
        assert_eq!(processor.counters().scheduled, 0);
    }

    #[test]
    fn causal_shift_never_reads_future_or_beyond_history() {
        for transient in (0..4096).step_by(4) {
            for translen in [0usize, 255] {
                let correction = ScheduledCorrection {
                    transient,
                    pre_noise: 256 + transient as usize % 256,
                    scaling_length: translen,
                };
                assert!(correction.shift() > LEADING_CROSSFADE);
                assert!(correction.shift() <= HISTORY_SAMPLES);
                assert!(correction.corrected_length() >= LEADING_CROSSFADE + TRAILING_CROSSFADE);
            }
        }
    }

    #[test]
    fn layout_change_and_discard_forget_pending_events() {
        let mut processor = TransientPreNoiseProcessor::new();
        let mut pcm = ramp(2 * 256);
        let original = pcm.clone();
        let run = |processor: &mut TransientPreNoiseProcessor,
                   pcm: &mut Vec<f32>,
                   positions: &[crate::output::ChannelPosition],
                   events: &[Option<TransientPreNoise>]| {
            processor.process_frame(
                PlanarFrame {
                    sample_rate_hz: 48_000,
                    positions,
                    samples_per_channel: 256,
                    planar: pcm,
                },
                events,
            );
        };
        // Событие на 4 кадра вперёд в кадре 2 канала L.
        run(&mut processor, &mut pcm, &[Left, Right], &[None, None]);
        let mut second = ramp(2 * 256);
        run(
            &mut processor,
            &mut second,
            &[Left, Right],
            &[Some(event(1400, 10)), None],
        );
        // Смена раскладки — событие и история забыты: кадр не меняется.
        let mut third = original.clone();
        run(&mut processor, &mut third, &[Left, Center], &[None, None]);
        assert_eq!(third, original);
        let mut fourth = original.clone();
        run(&mut processor, &mut fourth, &[Left, Center], &[None, None]);
        assert_eq!(fourth, original);
        // discard тоже забывает отложенное событие.
        run(
            &mut processor,
            &mut pcm,
            &[Left, Right],
            &[Some(event(1400, 10)), None],
        );
        processor.discard();
        let mut after = original.clone();
        run(&mut processor, &mut after, &[Left, Right], &[None, None]);
        assert_eq!(after, original);
    }
}
