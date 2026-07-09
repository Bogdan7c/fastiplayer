//! Атомарный pause/resume boundary concrete audio output-а.
//!
//! CPAL не обещает, что `pause()` сам по себе синхронизирован с последним
//! data callback-ом. Поэтому stream operation и clock transition дополняются
//! consumer mutex-ом, который уже сериализует callback read/publication.

use anyhow::{Context, Result};
use audio_core::AudioOutputClockTiming;
use cpal::traits::StreamTrait;
use tracing::{info, warn};

use super::{AudioOutput, BackendStreamState, PauseStreamOutcome, pause_stream_with_policy};

impl AudioOutput {
    /// Запускает stream и только после успеха возобновляет frozen clock.
    pub fn play(&mut self) -> Result<()> {
        if self.is_playing {
            return Ok(());
        }

        // Пока clock frozen, ранний callback после backend resume выдаст silence
        // и не возьмёт PCM. Не держим mutex во время `play`: некоторые backend-ы
        // синхронно запускают первый callback и иначе могли бы deadlock-нуться.
        if self.backend_stream_state == BackendStreamState::Paused {
            self.stream
                .play()
                .context("Не удалось запустить audio stream")?;
            self.backend_stream_state = BackendStreamState::Running;
        }
        let consumer = self
            .consumer
            .lock()
            .map_err(|error| anyhow::anyhow!("Audio buffer mutex poisoned: {error}"))?;
        self.clock.resume_output_timing();
        self.is_playing = true;
        drop(consumer);

        info!("Audio stream started");
        Ok(())
    }

    /// Ставит stream на паузу и возвращает тот же neutral clock snapshot,
    /// который останется видимым через `AudioClock` до следующего `play()`.
    pub fn pause_and_freeze_clock(&mut self) -> Result<AudioOutputClockTiming> {
        let pause_outcome = if self.is_playing {
            // Сначала просим backend остановиться. Затем mutex дожидается уже
            // начатого callback-а и не допускает новый к clock freeze.
            Some(pause_stream_with_policy(&self.stream)?)
        } else {
            None
        };

        let consumer = self
            .consumer
            .lock()
            .map_err(|error| anyhow::anyhow!("Audio buffer mutex poisoned: {error}"))?;
        let frozen_timing = self.clock.freeze_output_timing();
        match pause_outcome {
            Some(PauseStreamOutcome::Paused) => {
                self.backend_stream_state = BackendStreamState::Paused;
            }
            Some(PauseStreamOutcome::UnsupportedByBackend) => {
                self.backend_stream_state = BackendStreamState::Running;
            }
            None => {}
        }
        self.is_playing = false;
        drop(consumer);

        match pause_outcome {
            Some(PauseStreamOutcome::Paused) => info!("Audio stream paused"),
            Some(PauseStreamOutcome::UnsupportedByBackend) => warn!(
                "CPAL backend не поддерживает pause; callback переведён в logical silence gate"
            ),
            None => {}
        }

        Ok(frozen_timing)
    }
}
