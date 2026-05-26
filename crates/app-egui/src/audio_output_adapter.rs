use std::sync::{
    Arc,
    mpsc::{self, Sender, SyncSender},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, anyhow};
use player_core::{AudioOutputFactory, AudioOutputSpec, PlayerAudioClock, PlayerAudioOutput};
use tracing::warn;

/// Production factory, создающая concrete CPAL output внутри composition layer-а.
#[derive(Debug, Default)]
pub(crate) struct CpalAudioOutputFactory;

impl AudioOutputFactory for CpalAudioOutputFactory {
    /// Создаёт Send-handle, а concrete CPAL output оставляет на dedicated audio thread.
    fn create_output(&self, spec: AudioOutputSpec) -> anyhow::Result<Box<dyn PlayerAudioOutput>> {
        CpalPlayerAudioOutput::spawn(spec)
            .map(|output| Box::new(output) as Box<dyn PlayerAudioOutput>)
    }
}

/// Команды, которые player-core handle синхронно отправляет владельцу CPAL output-а.
enum AudioOutputThreadCommand {
    /// Записать PCM samples в ring buffer.
    WriteSamples {
        /// Owned samples, потому что команда пересекает thread boundary.
        samples: Vec<f32>,

        /// Ответ с количеством фактически записанных samples.
        reply: SyncSender<u64>,
    },

    /// Запустить CPAL stream.
    Play {
        /// Ответ с ошибкой backend-а, если stream не стартовал.
        reply: SyncSender<anyhow::Result<()>>,
    },

    /// Поставить CPAL stream на паузу.
    Pause {
        /// Ответ с ошибкой backend-а, если pause не удался.
        reply: SyncSender<anyhow::Result<()>>,
    },

    /// Очистить buffer/resampler state для seek generation.
    ClearBufferForSeek {
        /// Seek generation, которую должен подтвердить output.
        generation: u64,

        /// Ответ с ack generation или ошибкой очистки.
        reply: SyncSender<anyhow::Result<u64>>,
    },

    /// Обновить volume на owner thread-е.
    SetVolume {
        /// Volume уже валидируется/clamp-ится concrete output-ом.
        volume: f32,

        /// Ack нужен, чтобы caller не терял ordering относительно следующих samples.
        reply: SyncSender<()>,
    },

    /// Прочитать текущий уровень audio buffer-а.
    BufferLevelMs {
        /// Ответ с buffer level в миллисекундах.
        reply: SyncSender<f64>,
    },

    /// Завершить owner thread и drop-нуть concrete output на том же thread-е.
    Shutdown,
}

/// Send adapter, который не перемещает concrete `audio::AudioOutput` между потоками.
struct CpalPlayerAudioOutput {
    /// Канал команд к thread-у, владеющему CPAL stream.
    command_tx: Sender<AudioOutputThreadCommand>,

    /// Нейтральный clock handle, безопасный для чтения из player worker-а.
    clock: Arc<dyn PlayerAudioClock>,

    /// JoinHandle нужен, чтобы drop дождался освобождения CPAL stream.
    join_handle: Option<JoinHandle<()>>,
}

impl CpalPlayerAudioOutput {
    /// Создаёт owner thread, строит concrete output на нём и возвращает Send adapter.
    fn spawn(spec: AudioOutputSpec) -> anyhow::Result<Self> {
        let (command_tx, command_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);

        let join_handle = thread::Builder::new()
            .name("audio-output".into())
            .spawn(move || run_audio_output_thread(spec, command_rx, ready_tx))
            .context("failed to spawn audio output thread")?;

        let concrete_clock = match ready_rx.recv() {
            Ok(Ok(clock)) => clock,
            Ok(Err(error)) => {
                join_audio_output_thread(join_handle);
                return Err(error);
            }
            Err(error) => {
                join_audio_output_thread(join_handle);
                return Err(anyhow!("audio output thread stopped before init: {error}"));
            }
        };

        let clock: Arc<dyn PlayerAudioClock> = Arc::new(CpalPlayerAudioClock {
            clock: concrete_clock,
        });

        Ok(Self {
            command_tx,
            clock,
            join_handle: Some(join_handle),
        })
    }

    /// Отправляет command с one-shot reply и возвращает ошибку, если owner thread уже остановлен.
    fn request<Reply>(
        &self,
        build_command: impl FnOnce(SyncSender<Reply>) -> AudioOutputThreadCommand,
    ) -> anyhow::Result<Reply> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.command_tx
            .send(build_command(reply_tx))
            .context("audio output thread is stopped")?;
        reply_rx
            .recv()
            .context("audio output thread dropped command response")
    }
}

impl PlayerAudioOutput for CpalPlayerAudioOutput {
    /// Делегирует запись PCM в concrete ring buffer на owner thread-е.
    fn write_samples(&mut self, samples: &[f32]) -> u64 {
        self.request(|reply| AudioOutputThreadCommand::WriteSamples {
            samples: samples.to_vec(),
            reply,
        })
        .unwrap_or_else(|error| {
            warn!(error = %error, "Не удалось записать samples в audio output");
            0
        })
    }

    /// Делегирует запуск stream-а и сохраняет ошибку видимой для session policy.
    fn play(&mut self) -> anyhow::Result<()> {
        self.request(|reply| AudioOutputThreadCommand::Play { reply })?
    }

    /// Делегирует pause stream-а и сохраняет ошибку видимой для session policy.
    fn pause(&mut self) -> anyhow::Result<()> {
        self.request(|reply| AudioOutputThreadCommand::Pause { reply })?
    }

    /// Делегирует seek-clear в concrete output, который владеет buffer/resampler state.
    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        self.request(|reply| AudioOutputThreadCommand::ClearBufferForSeek { generation, reply })?
    }

    /// Делегирует volume update и логирует thread failure, потому что contract не возвращает error.
    fn set_volume(&mut self, volume: f32) {
        if let Err(error) =
            self.request(|reply| AudioOutputThreadCommand::SetVolume { volume, reply })
        {
            warn!(error = %error, "Не удалось применить volume к audio output");
        }
    }

    /// Возвращает concrete buffer level через neutral boundary.
    fn buffer_level_ms(&self) -> f64 {
        self.request(|reply| AudioOutputThreadCommand::BufferLevelMs { reply })
            .unwrap_or_else(|error| {
                warn!(error = %error, "Не удалось прочитать audio buffer level");
                0.0
            })
    }

    /// Возвращает neutral shared clock handle.
    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        Arc::clone(&self.clock)
    }
}

impl Drop for CpalPlayerAudioOutput {
    /// Останавливает owner thread, чтобы concrete CPAL stream drop случился на thread-е владельца.
    fn drop(&mut self) {
        let _ = self.command_tx.send(AudioOutputThreadCommand::Shutdown);
        if let Some(join_handle) = self.join_handle.take() {
            join_audio_output_thread(join_handle);
        }
    }
}

/// Adapter над `audio::AudioClock`, чтобы player-core не зависел от concrete clock type.
struct CpalPlayerAudioClock {
    /// Concrete audio clock остаётся owned/shared внутри audio crate.
    clock: Arc<audio::AudioClock>,
}

impl PlayerAudioClock for CpalPlayerAudioClock {
    /// Делегирует чтение playback позиции concrete clock-у.
    fn now(&self) -> Duration {
        self.clock.now()
    }

    /// Делегирует reset concrete clock-у.
    fn reset(&self) {
        self.clock.reset();
    }

    /// Делегирует underrun diagnostics concrete clock-у.
    fn underrun_callbacks(&self) -> u64 {
        self.clock.underrun_callbacks()
    }
}

/// Создаёт concrete output и обслуживает команды до shutdown/drop handle-а.
fn run_audio_output_thread(
    spec: AudioOutputSpec,
    command_rx: mpsc::Receiver<AudioOutputThreadCommand>,
    ready_tx: SyncSender<anyhow::Result<Arc<audio::AudioClock>>>,
) {
    let mut output = match audio::AudioOutput::new(spec.sample_rate, spec.channels) {
        Ok(output) => output,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };

    let clock = Arc::clone(output.clock());
    if ready_tx.send(Ok(clock)).is_err() {
        return;
    }

    while let Ok(command) = command_rx.recv() {
        match command {
            AudioOutputThreadCommand::WriteSamples { samples, reply } => {
                let _ = reply.send(output.write_samples(&samples));
            }
            AudioOutputThreadCommand::Play { reply } => {
                let _ = reply.send(output.play());
            }
            AudioOutputThreadCommand::Pause { reply } => {
                let _ = reply.send(output.pause());
            }
            AudioOutputThreadCommand::ClearBufferForSeek { generation, reply } => {
                let _ = reply.send(output.clear_buffer_for_seek(generation));
            }
            AudioOutputThreadCommand::SetVolume { volume, reply } => {
                output.set_volume(volume);
                let _ = reply.send(());
            }
            AudioOutputThreadCommand::BufferLevelMs { reply } => {
                let _ = reply.send(output.buffer_level_ms());
            }
            AudioOutputThreadCommand::Shutdown => break,
        }
    }
}

/// Ждёт owner thread и логирует panic без влияния на player-core error contract.
fn join_audio_output_thread(join_handle: JoinHandle<()>) {
    if join_handle.join().is_err() {
        warn!("audio output thread завершился panic-ом");
    }
}
