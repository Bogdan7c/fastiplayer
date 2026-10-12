//! Заполнение выходного буфера CPAL из ring buffer-а: чистая часть data callback-а.
//!
//! Вынесено из `AudioOutput::fill_buffer`, чтобы правило «что считать underrun-ом»
//! проверялось тестами без реального устройства (`cpal::OutputCallbackInfo` вне backend-а
//! создать нельзя, а чтение ring buffer-а от него не зависит).

use cpal::{FromSample, Sample};
use ringbuf::HeapCons;
use ringbuf::traits::{Consumer, Observer};

use super::convert_sample_for_output;
use crate::clock::AudioClock;

/// Итог одного data callback-а: сколько сэмплов реального PCM и сколько тишины выдано.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CallbackFill {
    /// Output удержан (до первого `play()` или на pause): буфер залит тишиной, ring buffer
    /// и clock не тронуты. Это НЕ underrun: данных никто не обещал, звук не запрошен.
    Held,
    /// Output играет: `played_samples` взяты из ring buffer-а, остаток — тишина.
    /// Любая `silence_samples > 0` при играющем output-е — настоящий underrun.
    Played {
        played_samples: u64,
        silence_samples: u64,
    },
}

/// Заполняет `data` из `consumer` целыми interleaved кадрами и сообщает итог.
///
/// Вызывается под consumer mutex-ом: решение «удержан ли output» и чтение ring buffer-а
/// обязаны быть атомарны относительно `play()`/`pause`, которые переключают clock под тем же
/// mutex-ом.
pub(super) fn fill_from_ring<T>(
    data: &mut [T],
    consumer: &mut HeapCons<f32>,
    clock: &AudioClock,
    channels: usize,
) -> CallbackFill
where
    T: Sample + FromSample<f32>,
{
    // Logical pause нужен даже для backend-ов без CPAL pause: callback выдаёт silence,
    // но не расходует ring PCM и clock. Тем же гейтом output удерживается сразу после
    // создания, пока владелец не вызвал `play()`.
    if clock.output_timing_is_frozen() {
        data.fill(T::EQUILIBRIUM);
        return CallbackFill::Held;
    }

    // Underrun не должен рвать interleaved кадр: чтение, оборванное посреди кадра,
    // сдвигает каналы всего последующего потока (звук «прыгает» между левым и правым).
    // Читаем целые кадры, неполный хвост оставляем в ring buffer до следующего callback.
    let channels = channels.max(1);
    let occupied = consumer.occupied_len();
    let frame_aligned_occupied = occupied - (occupied % channels);
    let to_fill = data.len().min(frame_aligned_occupied);

    let mut played_samples = 0u64;
    let mut silence_samples = 0u64;
    for sample in data[..to_fill].iter_mut() {
        match consumer.try_pop() {
            Some(value) => {
                *sample = convert_sample_for_output(value);
                played_samples += 1;
            }
            None => {
                *sample = T::EQUILIBRIUM;
                silence_samples += 1;
            }
        }
    }
    for sample in data[to_fill..].iter_mut() {
        *sample = T::EQUILIBRIUM;
        silence_samples += 1;
    }

    CallbackFill::Played {
        played_samples,
        silence_samples,
    }
}

#[cfg(test)]
mod tests {
    use ringbuf::HeapRb;
    use ringbuf::traits::{Observer, Producer, Split};

    use super::{CallbackFill, fill_from_ring};
    use crate::clock::AudioClock;

    /// Ring buffer на `capacity` сэмплов, заранее заполненный `samples`.
    fn ring_with(
        capacity: usize,
        samples: &[f32],
    ) -> (ringbuf::HeapProd<f32>, ringbuf::HeapCons<f32>) {
        let (mut producer, consumer) = HeapRb::<f32>::new(capacity).split();
        assert_eq!(producer.push_slice(samples), samples.len());
        (producer, consumer)
    }

    #[test]
    fn new_output_is_held_so_empty_ring_before_first_play_is_not_an_underrun() {
        // Новый output замораживает clock до `play()`; callback до этого — тишина без счёта.
        let clock = AudioClock::new(48_000, 2);
        let _initial_timing = clock.freeze_output_timing();
        let (_producer, mut consumer) = ring_with(64, &[]);
        let mut data = [1.0_f32; 8];

        let fill = fill_from_ring(&mut data, &mut consumer, &clock, 2);

        assert_eq!(fill, CallbackFill::Held);
        assert_eq!(data, [0.0; 8]);
    }

    #[test]
    fn held_output_keeps_pcm_in_ring_until_play() {
        // PCM, записанный до `play()` (prefill/preroll), не должен «проиграться вхолостую».
        let clock = AudioClock::new(48_000, 2);
        let _initial_timing = clock.freeze_output_timing();
        let (_producer, mut consumer) = ring_with(64, &[0.5; 8]);
        let mut data = [1.0_f32; 8];

        assert_eq!(
            fill_from_ring(&mut data, &mut consumer, &clock, 2),
            CallbackFill::Held
        );
        assert_eq!(consumer.occupied_len(), 8);

        clock.resume_output_timing();
        let fill = fill_from_ring(&mut data, &mut consumer, &clock, 2);

        assert_eq!(
            fill,
            CallbackFill::Played {
                played_samples: 8,
                silence_samples: 0
            }
        );
        assert_eq!(data, [0.5; 8]);
    }

    #[test]
    fn playing_output_with_empty_ring_reports_real_underrun() {
        let clock = AudioClock::new(48_000, 2);
        let (_producer, mut consumer) = ring_with(64, &[]);
        let mut data = [1.0_f32; 8];

        let fill = fill_from_ring(&mut data, &mut consumer, &clock, 2);

        assert_eq!(
            fill,
            CallbackFill::Played {
                played_samples: 0,
                silence_samples: 8
            }
        );
        assert_eq!(data, [0.0; 8]);
    }

    #[test]
    fn underrun_never_splits_interleaved_frame() {
        // 3 сэмпла в стерео = 1 целый кадр + хвост: хвост остаётся в ring buffer-е.
        let clock = AudioClock::new(48_000, 2);
        let (_producer, mut consumer) = ring_with(64, &[0.25, 0.25, 0.75]);
        let mut data = [1.0_f32; 6];

        let fill = fill_from_ring(&mut data, &mut consumer, &clock, 2);

        assert_eq!(
            fill,
            CallbackFill::Played {
                played_samples: 2,
                silence_samples: 4
            }
        );
        assert_eq!(data, [0.25, 0.25, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(consumer.occupied_len(), 1);
    }

    #[test]
    fn tail_is_played_fully_then_paused_output_is_held_without_underruns() {
        // F4: сценарий конца файла. Хвост доигрывается целиком (пока clock идёт),
        // затем пауза (`pause_and_freeze_clock` морозит clock) — все следующие callback-и
        // Held, ring не трогается, underrun-ов нет; resume снова даёт настоящую игру.
        let clock = AudioClock::new(48_000, 2);
        let (mut producer, mut consumer) = ring_with(64, &[0.5; 8]);
        let mut data = [1.0_f32; 8];

        assert_eq!(
            fill_from_ring(&mut data, &mut consumer, &clock, 2),
            CallbackFill::Played {
                played_samples: 8,
                silence_samples: 0
            },
            "tail should not be cut off"
        );
        assert_eq!(data, [0.5; 8]);

        let _frozen_timing = clock.freeze_output_timing();
        for _ in 0..100 {
            data.fill(1.0);
            assert_eq!(
                fill_from_ring(&mut data, &mut consumer, &clock, 2),
                CallbackFill::Held
            );
            assert_eq!(data, [0.0; 8]);
        }

        // Replay/seek после Ended: resume + новый PCM играют как обычно.
        assert_eq!(producer.push_slice(&[0.25; 8]), 8);
        clock.resume_output_timing();
        assert_eq!(
            fill_from_ring(&mut data, &mut consumer, &clock, 2),
            CallbackFill::Played {
                played_samples: 8,
                silence_samples: 0
            }
        );
        assert_eq!(data, [0.25; 8]);
    }
}
