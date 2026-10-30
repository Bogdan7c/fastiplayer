//! Задержка очереди устройства: звук, отданный callback-ом, слышен не сразу.
//!
//! Каждый CPAL callback дописывает PCM в конец очереди устройства (у ALSA — кольцевой
//! буфер драйвера). Услышан этот PCM будет только после всего, что уже стоит в очереди.
//! CPAL сообщает это время как `timestamp.playback − timestamp.callback`, но не везде:
//! через `pipewire-alsa` (Linux по умолчанию) задержка приходит нулём, и часы считали,
//! что звук слышен в момент callback-а. Очередь там ~100 мс (4800 кадров), callback
//! дописывает 32 мс, значит записанное слышно через ~68 мс. Часы на эти 68 мс бежали
//! впереди звука, а картинка, идущая по часам, — тоже: звук отставал от видео.
//!
//! Почему дефект выглядел как «звук раньше картинки до первого seek» (сессия 03e моста):
//! если первый callback с PCM заполнял пустую очередь целиком (100 мс), следующие anchor-ы
//! цеплялись за его конец (`chain_reported_playback_ns`) и случайно получали верные
//! +68 мс. Seek сбрасывает anchor-ы (`AudioClock::reset`), и задержка терялась. Какой
//! callback окажется первым с PCM, решали гонки старта, поэтому сдвиг то был, то нет.
//! Эталон mpv на том же стенде совпал с состоянием «с задержкой», то есть неправым было
//! состояние после seek, а не до него.
//!
//! Решение: оценивать очередь самим. CPAL ALSA-backend отдаёт в callback ровно столько
//! кадров, сколько в очереди свободно (`avail`), поэтому:
//! - ёмкость очереди = самый большой callback (первый callback пустой очереди равен ей);
//! - занято перед этим callback-ом = ёмкость − его размер;
//! - задержка = max(сообщённая CPAL, занятое / частота).
//!
//! Ёмкость — свойство устройства, а не медиа: `reset` (seek) её не сбрасывает — старый
//! звук в очереди устройства действительно доиграет перед новым. Новый output (новый
//! stream) создаёт новые часы и узнаёт ёмкость с первого callback-а заново.
//! Backend, сообщающий настоящую задержку, не меняется: она включает очередь, а берётся
//! максимум, поэтому двойного счёта нет.
//!
//! Только ALSA ([`DeviceQueueModel::for_host`]). Модель «размер callback-а = свободное
//! место» проверена по исходникам `cpal-0.15.3/src/host/alsa/mod.rs` (`process_output`
//! пишет ровно `avail` кадров). У JACK, CoreAudio, ASIO и WASAPI размер callback-а — это
//! размер буфера, а не свободное место: ёмкость, растущая только вверх, там застряла бы
//! (буфер 4096 → 512 кадров дал бы лишние +75 мс навсегда). Там остаётся только задержка
//! от CPAL, как было раньше.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::{AudioClock, samples_to_nanos};

/// Что размер callback-а говорит об очереди устройства на этом CPAL-хосте.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeviceQueueModel {
    /// ALSA: callback заполняет всё свободное место очереди — по нему оцениваем занятое.
    CallbackFillsFreeSpace,
    /// Другие хосты: размер callback-а об очереди ничего не говорит, верим только CPAL.
    BackendReportedOnly,
}

impl DeviceQueueModel {
    /// Модель для хоста, на котором открыт output (см. модуль: почему только ALSA).
    pub(crate) fn for_host(host_id: cpal::HostId) -> Self {
        #[cfg(any(
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd"
        ))]
        if host_id == cpal::HostId::Alsa {
            return Self::CallbackFillsFreeSpace;
        }
        // На платформах без ALSA `host_id` дальше не нужен: оценки там нет.
        let _ = host_id;
        Self::BackendReportedOnly
    }
}

/// Оценка ёмкости очереди устройства по наблюдённым callback-ам.
///
/// Пишется из realtime callback-а, поэтому только атомик: без блокировок и аллокаций.
pub(super) struct DeviceQueueCapacity {
    /// Можно ли вообще судить об очереди по размеру callback-а.
    model: DeviceQueueModel,
    /// Самый большой callback в interleaved сэмплах (PCM + тишина); ноль — ещё не видели.
    largest_callback_samples: AtomicU64,
}

impl DeviceQueueCapacity {
    /// Без оценки очереди: только задержка от CPAL (умолчание `AudioClock::new`).
    pub(super) const fn backend_reported_only() -> Self {
        Self::new(DeviceQueueModel::BackendReportedOnly)
    }

    /// Ёмкость пока неизвестна: оценка задержки нулевая до первого callback-а.
    const fn new(model: DeviceQueueModel) -> Self {
        Self {
            model,
            largest_callback_samples: AtomicU64::new(0),
        }
    }

    /// Учитывает размер очередного callback-а (любого, в том числе удержанного тишиной).
    fn observe_callback(&self, callback_samples: u64) {
        self.largest_callback_samples
            .fetch_max(callback_samples, Ordering::Relaxed);
    }

    /// Сколько interleaved сэмплов стояло в очереди перед callback-ом такого размера.
    ///
    /// Без модели ALSA оценки нет (ноль): решает только задержка от CPAL.
    fn queued_before_callback(&self, callback_samples: u64) -> u64 {
        if self.model == DeviceQueueModel::BackendReportedOnly {
            return 0;
        }
        queued_before_callback(
            self.largest_callback_samples.load(Ordering::Relaxed),
            callback_samples,
        )
    }
}

impl AudioClock {
    /// Общие (output + callback) часы output-а, открытого на CPAL-хосте `host`.
    ///
    /// Оценка очереди устройства включается только на ALSA (см. модуль).
    pub(crate) fn shared_for_output_host(
        host: &cpal::Host,
        sample_rate: u32,
        channels: u32,
    ) -> std::sync::Arc<Self> {
        let model = DeviceQueueModel::for_host(host.id());
        std::sync::Arc::new(Self::with_device_queue_model(sample_rate, channels, model))
    }

    /// Часы с явно заданной моделью очереди устройства.
    pub(crate) fn with_device_queue_model(
        sample_rate: u32,
        channels: u32,
        model: DeviceQueueModel,
    ) -> Self {
        let mut clock = Self::new(sample_rate, channels);
        clock.device_queue = DeviceQueueCapacity::new(model);
        clock
    }

    /// Учитывает размер output callback-а в оценке ёмкости очереди устройства.
    ///
    /// Вызывается для каждого callback-а, включая удержанные до `play()`: их тишина тоже
    /// занимает очередь, и первый (самый полный) callback часто приходится именно на них.
    pub(crate) fn observe_output_callback_size(&self, callback_samples: u64) {
        self.device_queue.observe_callback(callback_samples);
    }

    /// Через сколько после callback-а прозвучит его первый сэмпл.
    ///
    /// `reported_delay` — то, что сообщил backend (может быть нулём, см. модуль);
    /// `callback_output_samples` — размер этого callback-а (PCM + тишина).
    pub(crate) fn playback_delay(
        &self,
        reported_delay: Duration,
        callback_output_samples: u64,
    ) -> Duration {
        let queued_samples = self
            .device_queue
            .queued_before_callback(callback_output_samples);
        let queued_delay =
            Duration::from_nanos(samples_to_nanos(queued_samples, self.samples_per_second()));
        effective_playback_delay(reported_delay, queued_delay)
    }
}

/// Задержка, которую сообщил CPAL: `playback − callback` (ноль, если backend её не знает).
pub(crate) fn reported_playback_delay(callback_info: &cpal::OutputCallbackInfo) -> Duration {
    let timestamp = callback_info.timestamp();
    timestamp
        .playback
        .duration_since(&timestamp.callback)
        .unwrap_or(Duration::ZERO)
}

/// Занято в очереди перед callback-ом: ёмкость минус свободное место, которое он заполнил.
///
/// Неизвестная ёмкость (ноль) и callback не меньше ёмкости дают ноль: очередь пуста.
fn queued_before_callback(capacity_samples: u64, callback_samples: u64) -> u64 {
    capacity_samples.saturating_sub(callback_samples)
}

/// Итоговая задержка: сообщённая backend-ом или оценка по очереди — что больше.
///
/// Сообщённая задержка включает и очередь, и аппаратную часть, поэтому у честного
/// backend-а она не меньше оценки; ноль от backend-а означает «не знаю», а не «сразу».
fn effective_playback_delay(reported_delay: Duration, queued_delay: Duration) -> Duration {
    reported_delay.max(queued_delay)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use super::{DeviceQueueModel, effective_playback_delay, queued_before_callback};
    use crate::clock::{AudioClock, PlaybackAnchor, interpolate_anchor_samples};

    /// Очередь `pipewire-alsa` со стенда: 4800 кадров стерео (100 мс при 48 кГц).
    const DEVICE_QUEUE_SAMPLES: u64 = 9_600;
    /// Обычный callback со стенда: 1536 кадров стерео (32 мс).
    const REGULAR_CALLBACK_SAMPLES: u64 = 3_072;
    /// Сколько ждёт PCM обычного callback-а: (4800 − 1536) кадров = 68 мс.
    const QUEUED_DELAY: Duration = Duration::from_millis(68);
    /// Interleaved сэмплов в секунду (48 кГц стерео).
    const SAMPLES_PER_SECOND: u64 = 96_000;

    /// Часы ALSA-output-а со стенда.
    fn alsa_clock() -> AudioClock {
        AudioClock::with_device_queue_model(48_000, 2, DeviceQueueModel::CallbackFillsFreeSpace)
    }

    /// Момент «через `offset` после создания часов»: время теста не зависит от планировщика.
    fn at(clock: &AudioClock, offset: Duration) -> Instant {
        clock.origin + offset
    }

    /// Задержка свежего anchor-а относительно callback-а, случившегося в `callback_at`.
    fn latest_anchor_delay(clock: &AudioClock, callback_at: Instant) -> Duration {
        Duration::from_nanos(
            clock.playback_anchor_ns.load(Ordering::Relaxed)
                - clock.logical_origin_ns_for_instant(callback_at),
        )
    }

    /// Слышимая позиция (interleaved сэмплы) по свежему anchor-у в момент `listened_at`.
    fn audible_samples_at(clock: &AudioClock, listened_at: Instant) -> u64 {
        let anchor = PlaybackAnchor {
            playback_ns: clock.playback_anchor_ns.load(Ordering::Relaxed),
            start_samples: clock.playback_anchor_samples.load(Ordering::Relaxed),
            end_samples: clock.playback_anchor_end_samples.load(Ordering::Relaxed),
            output_end_ns: clock.playback_anchor_output_end_ns.load(Ordering::Relaxed),
        };
        interpolate_anchor_samples(
            anchor,
            clock.logical_origin_ns_for_instant(listened_at),
            SAMPLES_PER_SECOND,
        )
    }

    #[test]
    fn queue_before_callback_is_capacity_minus_the_free_space_it_fills() {
        assert_eq!(
            queued_before_callback(DEVICE_QUEUE_SAMPLES, REGULAR_CALLBACK_SAMPLES),
            6_528
        );
        assert_eq!(
            queued_before_callback(DEVICE_QUEUE_SAMPLES, DEVICE_QUEUE_SAMPLES),
            0,
            "the first callback of an empty queue fills all of it and plays right away"
        );
        assert_eq!(
            queued_before_callback(0, REGULAR_CALLBACK_SAMPLES),
            0,
            "unknown capacity estimates nothing"
        );
    }

    #[test]
    fn honest_backend_delay_wins_and_zero_means_unknown() {
        let honest = Duration::from_millis(90);
        assert_eq!(effective_playback_delay(honest, QUEUED_DELAY), honest);
        assert_eq!(
            effective_playback_delay(Duration::ZERO, QUEUED_DELAY),
            QUEUED_DELAY
        );
    }

    #[test]
    fn only_the_alsa_host_estimates_the_queue_from_callback_sizes() {
        #[cfg(target_os = "linux")]
        assert_eq!(
            DeviceQueueModel::for_host(cpal::HostId::Alsa),
            DeviceQueueModel::CallbackFillsFreeSpace
        );
        assert_eq!(
            DeviceQueueModel::for_host(cpal::default_host().id()),
            if cfg!(target_os = "linux") {
                DeviceQueueModel::CallbackFillsFreeSpace
            } else {
                DeviceQueueModel::BackendReportedOnly
            }
        );
    }

    #[test]
    fn backend_reporting_zero_gets_the_queue_delay_after_one_full_callback() {
        let clock = alsa_clock();
        assert_eq!(
            clock.playback_delay(Duration::ZERO, REGULAR_CALLBACK_SAMPLES),
            Duration::ZERO,
            "before any callback the queue is unknown"
        );

        clock.observe_output_callback_size(DEVICE_QUEUE_SAMPLES);
        clock.observe_output_callback_size(REGULAR_CALLBACK_SAMPLES);

        assert_eq!(
            clock.playback_delay(Duration::ZERO, REGULAR_CALLBACK_SAMPLES),
            QUEUED_DELAY
        );
    }

    /// Честный backend уже включил очередь в свою задержку: оценка не прибавляется к ней.
    #[test]
    fn honest_reported_delay_is_used_as_is_without_double_counting_the_queue() {
        let clock = alsa_clock();
        let honest = Duration::from_millis(90);
        clock.observe_output_callback_size(DEVICE_QUEUE_SAMPLES);
        let callback_at = at(&clock, Duration::from_millis(500));
        clock.observe_output_callback_size(REGULAR_CALLBACK_SAMPLES);
        clock.record_output_callback(REGULAR_CALLBACK_SAMPLES, 0, honest, callback_at);

        assert_eq!(latest_anchor_delay(&clock, callback_at), honest);
        assert_eq!(
            audible_samples_at(&clock, callback_at + Duration::from_millis(89)),
            0,
            "the callback is not heard before the reported delay"
        );
        assert_eq!(
            audible_samples_at(&clock, callback_at + honest + Duration::from_millis(32)),
            REGULAR_CALLBACK_SAMPLES,
            "and is fully heard one callback later, not 68 ms later still"
        );
    }

    /// Не ALSA: размер callback-а — размер буфера. Скачок 4096 → 512 кадров не должен
    /// превратиться в «очередь 3584 кадра» (+75 мс навсегда).
    #[test]
    fn non_alsa_host_ignores_callback_size_jumps() {
        let clock = AudioClock::new(48_000, 2);
        let callback_at = at(&clock, Duration::from_millis(500));
        clock.observe_output_callback_size(8_192);
        clock.observe_output_callback_size(1_024);
        clock.record_output_callback(1_024, 0, Duration::ZERO, callback_at);

        assert_eq!(clock.playback_delay(Duration::ZERO, 1_024), Duration::ZERO);
        assert_eq!(latest_anchor_delay(&clock, callback_at), Duration::ZERO);
    }

    /// Главный сценарий дефекта: seek сбросил часы, а в очереди устройства ещё 68 мс
    /// старого звука. Новый PCM прозвучит только после них — до тех пор слышимая
    /// позиция нового отрезка ноль (иначе картинка уходит вперёд звука).
    #[test]
    fn audio_after_seek_reset_is_heard_only_after_the_queued_device_audio() {
        let clock = alsa_clock();
        let started = at(&clock, Duration::from_millis(100));
        clock.observe_output_callback_size(DEVICE_QUEUE_SAMPLES);
        clock.record_output_callback(DEVICE_QUEUE_SAMPLES, 0, Duration::ZERO, started);

        clock.reset();

        // Первый callback после seek дописал 32 мс нового PCM за 68 мс старого.
        let callback_at = at(&clock, Duration::from_millis(1_000));
        clock.observe_output_callback_size(REGULAR_CALLBACK_SAMPLES);
        clock.record_output_callback(REGULAR_CALLBACK_SAMPLES, 0, Duration::ZERO, callback_at);

        assert_eq!(latest_anchor_delay(&clock, callback_at), QUEUED_DELAY);
        assert_eq!(
            audible_samples_at(&clock, callback_at + Duration::from_millis(30)),
            0,
            "new audio is still behind the old one in the device queue"
        );
        assert_eq!(
            audible_samples_at(
                &clock,
                callback_at + QUEUED_DELAY + Duration::from_millis(16)
            ),
            REGULAR_CALLBACK_SAMPLES / 2,
            "half of the new callback is heard 16 ms after the old audio ran out"
        );
    }

    /// Состояние до seek и после совпадают: задержка не зависит от того, был ли первый
    /// callback с PCM полным (раньше только он случайно давал верные +68 мс).
    #[test]
    fn queue_delay_is_the_same_whether_or_not_the_first_pcm_callback_was_full() {
        let full_first = alsa_clock();
        let start = at(&full_first, Duration::from_millis(100));
        full_first.observe_output_callback_size(DEVICE_QUEUE_SAMPLES);
        full_first.record_output_callback(DEVICE_QUEUE_SAMPLES, 0, Duration::ZERO, start);
        let second = start + Duration::from_millis(32);
        full_first.observe_output_callback_size(REGULAR_CALLBACK_SAMPLES);
        full_first.record_output_callback(REGULAR_CALLBACK_SAMPLES, 0, Duration::ZERO, second);

        // Полный первый callback ушёл на удержанную тишину (без anchor-а).
        let silent_first = alsa_clock();
        let silent_second = at(&silent_first, Duration::from_millis(132));
        silent_first.observe_output_callback_size(DEVICE_QUEUE_SAMPLES);
        silent_first.observe_output_callback_size(REGULAR_CALLBACK_SAMPLES);
        silent_first.record_output_callback(
            REGULAR_CALLBACK_SAMPLES,
            0,
            Duration::ZERO,
            silent_second,
        );

        assert_eq!(latest_anchor_delay(&full_first, second), QUEUED_DELAY);
        assert_eq!(
            latest_anchor_delay(&silent_first, silent_second),
            QUEUED_DELAY
        );
    }
}
