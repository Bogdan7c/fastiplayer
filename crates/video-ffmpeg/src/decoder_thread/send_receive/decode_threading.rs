//! Политика потоков software-декода по нагрузке потока (cpu-parity сессия 08).
//!
//! `video-core` знает только намерение: `SoftwareDecodeThreadBudget::Auto` («подобрать
//! самому», общий запас «ядра − 2» под render/upload/worker) или `Fixed(N)` (ручной
//! `video.sw_decode_threads`). Что значит «поток» для конкретного декодера, знает
//! только FFmpeg-backend, поэтому кодек-зависимая часть живёт здесь:
//!
//! - H.264/HEVC/VP9/VP8 в libavcodec работают frame threading-ом: каждый поток —
//!   отдельный кадр. Суммарный CPU от числа потоков не зависит, а перемотка быстрее
//!   с большим числом потоков. Решение владельца (22.10.2026): оставить «ядра − 2».
//! - AV1 декодирует libdav1d с малым `max_frame_delay` (1–2 кадра в полёте): потоки
//!   делят эти кадры между собой, и лишние потоки только толкаются на синхронизации.
//!   Замер на 4K60 (при `max_frame_delay = 1`): 14 потоков — 165 % CPU процесса,
//!   6 потоков — 103 %, при тех же 60 кадрах/с. Решение владельца: AV1 до 1080p —
//!   не больше 4 потоков, выше — не больше 6.
//! - Тот же `max_frame_delay = 1` на одно-тайловом 4K60 AV1 (SVT-AV1) упирается в
//!   последовательный разбор тайла: декод ~60 кадров/с, плеер показывает 831 из 1200
//!   кадров. `max_frame_delay = 2` даёт 1200 из 1200 (+106 МиБ RSS, CPU тот же).
//!   Решение владельца (22.10.2026): 2 для AV1 выше 1080p (и неизвестного размера),
//!   до 1080p остаётся 1. Подробности — [`SoftwareDecodeWorkload::max_frame_delay`].
//! - **Слабая машина** (не больше 4 логических CPU, эталон — ThinkPad T410, 2 ядра /
//!   4 потока; [`HostCpuProfile`]) — два исключения (решение владельца 26.10.2026,
//!   подсессия 11a плана `user/gles2-renderer/`):
//!   - AV1 до 1080p получает `max_frame_delay = 2`: с одним кадром в полёте dav1d на
//!     T410 декодирует AV1 1080p60 всего ~29 кадров/с (плеер показывает ~15), с двумя —
//!     ~80 кадров/с;
//!   - HEVC в `Auto` получает все логические CPU и ещё два потока, а не «ядра − 2» (на T410
//!     = 2 потока): HEVC 1080p60 с 2 потоками — 56 кадров/с, с 4 — 60 при +20 п.п. CPU
//!     (11a). На тяжёлом HEVC 1080p60 25 Мбит/с 4 потока теряли 53 кадра за окно 20 с, а 6 —
//!     ни одного при том же CPU (сессия 12a, решение владельца 67): лишние потоки frame
//!     threading-а прячут ожидание на гипертрединге, как у VLC и mpv (те берут ядра + 1).
//!
//!   H.264 и VP9 на слабой машине не меняются: 2 потоков им хватает на 1080p60, а
//!   каждый лишний поток там только добавляет 3–5 п.п. CPU на синхронизации.
//!
//! Модуль чистый (без FFmpeg-типов), чтобы правило проверялось тестами без FFmpeg.

use std::num::NonZeroUsize;

use video_core::SoftwareDecodeThreadBudget;

use crate::codec_adapter::FfmpegDecoderId;
use crate::ffi::codec_context::FfmpegCodecContextRequest;

/// `NonZeroUsize` из литерала в const-контексте без `unwrap` (политика паник проекта).
const fn thread_cap(count: usize) -> NonZeroUsize {
    match NonZeroUsize::new(count) {
        Some(count) => count,
        None => NonZeroUsize::MIN,
    }
}

/// Наибольшая площадь кадра, которая ещё считается «1080p» для политики AV1.
///
/// 1920×1088 — coded size 1080p-потока (высота выровнена до 16 у многих энкодеров).
const AV1_UP_TO_1080P_MAX_FRAME_AREA: u64 = 1920 * 1088;

/// Потолок потоков dav1d для AV1 до 1080p включительно: на 1080p60 даже 2 потока
/// декодируют в 3+ раза быстрее реального времени, CPU с 4 потоками на 2–3 п.п. ниже.
const AV1_UP_TO_1080P_THREAD_CAP: NonZeroUsize = thread_cap(4);

/// Потолок потоков dav1d для AV1 выше 1080p (4K). Замер при `max_frame_delay = 1`:
/// 103 % CPU против 165 % у 14 потоков, перемотка на файле с GOP 4–5 с — 0,53 с против
/// 0,36 с. С итоговым `max_frame_delay = 2` тот же файл — ~115 % CPU.
const AV1_ABOVE_1080P_THREAD_CAP: NonZeroUsize = thread_cap(6);

/// `max_frame_delay` libdav1d для AV1 до 1080p включительно (Session 29): один кадр в
/// полёте убирает всплески выдачи кадров, а 1080p60 декодируется с большим запасом.
const AV1_UP_TO_1080P_MAX_FRAME_DELAY: u32 = 1;

/// `max_frame_delay` libdav1d для AV1 выше 1080p. Замер (20 с, 4K60 single-tile):
/// 1 → 831/1200 кадров, 2 → 1200/1200, 0 (auto) → 1200/1200, но на 45 МиБ RSS больше.
/// Цена 2: +100…190 МиБ RSS на 4K и +12 % CPU на много-тайловых/HDR 4K60.
const AV1_ABOVE_1080P_MAX_FRAME_DELAY: u32 = 2;

/// `max_frame_delay` libdav1d для AV1 до 1080p на слабой машине. Замер на T410
/// (ffmpeg + libdav1d, только декод, AV1 1080p60): 1 → 29,9 кадра/с, 2 → 79,8 кадра/с
/// при двух потоках. Всплески выдачи, ради которых на мощной машине стоит 1, здесь
/// вторичны: без второго кадра в полёте видео просто не успевает.
const AV1_UP_TO_1080P_CONSTRAINED_HOST_MAX_FRAME_DELAY: u32 = 2;

/// Наибольшее число логических CPU, при котором машина считается слабой.
///
/// 4 — это 2 ядра с Hyper-Threading (T410) или 4 простых ядра: «ядра − 2» оставляет
/// таким машинам всего 2 потока декода, и тяжёлым кодекам их не хватает.
const CONSTRAINED_HOST_MAX_LOGICAL_CPUS: usize = 4;

/// Сколько потоков HEVC получает сверх числа логических CPU на слабой машине (решение
/// владельца 67, сессия 12a). Цена — по кадру в работе на поток (~7–10 МиБ на 1080p).
const CONSTRAINED_HOST_HEVC_EXTRA_THREADS: usize = 2;

/// Процессор машины, на которой открывается декодер, — вход политики потоков.
///
/// Отдельный тип, а не чтение `available_parallelism` внутри правил: политику можно
/// проверить тестами для любой машины, а не только для той, где идут тесты.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HostCpuProfile {
    /// Число логических CPU (ядра × потоки на ядро), как его видит процесс.
    logical_cpus: NonZeroUsize,
}

impl HostCpuProfile {
    /// Профиль текущей машины. Если ОС не сообщила число CPU, считаем машину
    /// слабой с одним CPU: это безопасная сторона (меньше потоков, больше задержка AV1).
    pub(super) fn detect() -> Self {
        Self::with_logical_cpus(std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN))
    }

    /// Профиль машины с заданным числом логических CPU.
    pub(super) const fn with_logical_cpus(logical_cpus: NonZeroUsize) -> Self {
        Self { logical_cpus }
    }

    /// Слабая ли машина (см. [`CONSTRAINED_HOST_MAX_LOGICAL_CPUS`]).
    fn is_constrained(self) -> bool {
        self.logical_cpus.get() <= CONSTRAINED_HOST_MAX_LOGICAL_CPUS
    }
}

/// Coded size потока, если demux его знает.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodedFrameSize {
    /// Ширина и высота известны из конфигурации потока.
    Known { width: u32, height: u32 },

    /// Размер неизвестен (контейнер его не сообщил).
    Unknown,
}

impl CodedFrameSize {
    /// Собирает размер из optional полей `VideoStreamDecodeConfig`.
    pub(super) fn from_optional(width: Option<u32>, height: Option<u32>) -> Self {
        match (width, height) {
            (Some(width), Some(height)) => Self::Known { width, height },
            _ => Self::Unknown,
        }
    }

    /// Укладывается ли кадр в «1080p»; неизвестный размер — нет, чтобы не урезать 4K.
    fn fits_1080p(self) -> bool {
        match self {
            Self::Known { width, height } => {
                u64::from(width) * u64::from(height) <= AV1_UP_TO_1080P_MAX_FRAME_AREA
            }
            Self::Unknown => false,
        }
    }
}

/// Нагрузка потока, от которой зависит выбор числа потоков декода.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SoftwareDecodeWorkload {
    /// Декодер, который откроет adapter.
    pub(super) decoder: FfmpegDecoderId,

    /// Coded size потока.
    pub(super) coded_size: CodedFrameSize,
}

impl SoftwareDecodeWorkload {
    /// Желаемый `max_frame_delay` декодера; `None` — оставить умолчание FFmpeg.
    ///
    /// Применяется только к libdav1d (AVOption `max_frame_delay` пробрасывается лишь
    /// для него). dav1d ограничивает число параллельных кадров числом потоков, поэтому
    /// при ручном `sw_decode_threads = 1` задержка 2 фактически становится 1 — отдельной
    /// обработки не требуется.
    ///
    /// AV1 до 1080p на слабой машине получает 2, а не 1 (см. документацию модуля).
    fn max_frame_delay(self, host: HostCpuProfile) -> Option<u32> {
        match self.decoder {
            FfmpegDecoderId::Av1 if self.coded_size.fits_1080p() && host.is_constrained() => {
                Some(AV1_UP_TO_1080P_CONSTRAINED_HOST_MAX_FRAME_DELAY)
            }
            FfmpegDecoderId::Av1 if self.coded_size.fits_1080p() => {
                Some(AV1_UP_TO_1080P_MAX_FRAME_DELAY)
            }
            FfmpegDecoderId::Av1 => Some(AV1_ABOVE_1080P_MAX_FRAME_DELAY),
            FfmpegDecoderId::Vp9
            | FfmpegDecoderId::H264
            | FfmpegDecoderId::Hevc
            | FfmpegDecoderId::Vp8 => None,
        }
    }

    /// Дописывает в запрос на открытие codec context-а настройки по нагрузке потока:
    /// frame delay и разрешённый (`Fixed`) thread budget. `host` — машина, на которой
    /// открывается декодер (на слабой правила другие, см. документацию модуля).
    pub(super) fn apply_to_request(
        self,
        mut request: FfmpegCodecContextRequest,
        thread_budget: SoftwareDecodeThreadBudget,
        host: HostCpuProfile,
    ) -> FfmpegCodecContextRequest {
        if let Some(max_frame_delay) = self.max_frame_delay(host) {
            request = request.with_max_frame_delay(max_frame_delay);
        }
        request.with_software_decode_thread_budget(resolve_decode_thread_budget(
            thread_budget,
            self,
            host,
        ))
    }

    /// Получает ли поток в `Auto` все логические CPU слабой машины (плюс запас
    /// [`CONSTRAINED_HOST_HEVC_EXTRA_THREADS`]) вместо «ядра − 2».
    ///
    /// Только HEVC: ему на слабой машине двух потоков не хватает уже на 1080p60, а
    /// H.264/VP9 укладываются и платили бы лишним CPU (замеры — в документации модуля).
    fn takes_all_cpus_on_constrained_host(self) -> bool {
        match self.decoder {
            FfmpegDecoderId::Hevc => true,
            FfmpegDecoderId::Av1
            | FfmpegDecoderId::Vp9
            | FfmpegDecoderId::H264
            | FfmpegDecoderId::Vp8 => false,
        }
    }

    /// Потолок потоков, если он у декодера есть; `None` — общий «ядра − 2».
    fn auto_thread_cap(self) -> Option<NonZeroUsize> {
        match self.decoder {
            FfmpegDecoderId::Av1 if self.coded_size.fits_1080p() => {
                Some(AV1_UP_TO_1080P_THREAD_CAP)
            }
            FfmpegDecoderId::Av1 => Some(AV1_ABOVE_1080P_THREAD_CAP),
            FfmpegDecoderId::Vp9
            | FfmpegDecoderId::H264
            | FfmpegDecoderId::Hevc
            | FfmpegDecoderId::Vp8 => None,
        }
    }
}

/// Разрешает budget потока в конкретный `Fixed(N)` для открытия codec context-а.
///
/// `Fixed` (ручной `video.sw_decode_threads`) передаётся как есть: это осознанный
/// override пользователя. `Auto` получает общий запас «ядра − 2» из `video-core` и
/// кодек-зависимый потолок из [`SoftwareDecodeWorkload`]; HEVC на слабой машине —
/// все логические CPU и ещё два.
fn resolve_decode_thread_budget(
    budget: SoftwareDecodeThreadBudget,
    workload: SoftwareDecodeWorkload,
    host: HostCpuProfile,
) -> SoftwareDecodeThreadBudget {
    match budget {
        SoftwareDecodeThreadBudget::Fixed(_) => budget,
        SoftwareDecodeThreadBudget::Auto => SoftwareDecodeThreadBudget::fixed(auto_thread_count(
            budget.resolved_thread_count(),
            workload,
            host,
        )),
    }
}

/// Число потоков для `Auto`: запас «ядра − 2», урезанный потолком декодера.
///
/// Потолок только уменьшает число: на слабой машине (4 логических CPU → 2 потока)
/// AV1 получает те же 2 потока, что и раньше. Исключение — HEVC на слабой машине:
/// он получает все логические CPU (`host`) и ещё [`CONSTRAINED_HOST_HEVC_EXTRA_THREADS`],
/// потому что запаса «ядра − 2» ему не хватает.
fn auto_thread_count(
    host_headroom_threads: NonZeroUsize,
    workload: SoftwareDecodeWorkload,
    host: HostCpuProfile,
) -> NonZeroUsize {
    if host.is_constrained() && workload.takes_all_cpus_on_constrained_host() {
        return host
            .logical_cpus
            .saturating_add(CONSTRAINED_HOST_HEVC_EXTRA_THREADS)
            .max(host_headroom_threads);
    }
    match workload.auto_thread_cap() {
        Some(cap) => host_headroom_threads.min(cap),
        None => host_headroom_threads,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::pixel_format::SoftwarePixelFormatSet;

    /// Мощная машина (780M: 16 логических CPU) — прежние правила cpu-parity 08.
    const AMPLE_HOST: HostCpuProfile = HostCpuProfile::with_logical_cpus(thread_cap(16));

    /// Слабая машина: ThinkPad T410, 2 ядра / 4 потока.
    const T410_HOST: HostCpuProfile = HostCpuProfile::with_logical_cpus(thread_cap(4));

    fn threads(count: usize) -> NonZeroUsize {
        NonZeroUsize::new(count).expect("test thread count is positive")
    }

    fn workload(decoder: FfmpegDecoderId, width: u32, height: u32) -> SoftwareDecodeWorkload {
        SoftwareDecodeWorkload {
            decoder,
            coded_size: CodedFrameSize::Known { width, height },
        }
    }

    #[test]
    fn av1_auto_threads_are_capped_by_frame_size() {
        let host = threads(14);
        assert_eq!(
            auto_thread_count(host, workload(FfmpegDecoderId::Av1, 1920, 1080), AMPLE_HOST),
            threads(4)
        );
        assert_eq!(
            auto_thread_count(host, workload(FfmpegDecoderId::Av1, 1920, 1088), AMPLE_HOST),
            threads(4)
        );
        assert_eq!(
            auto_thread_count(host, workload(FfmpegDecoderId::Av1, 1280, 720), AMPLE_HOST),
            threads(4)
        );
        assert_eq!(
            auto_thread_count(host, workload(FfmpegDecoderId::Av1, 2560, 1440), AMPLE_HOST),
            threads(6)
        );
        assert_eq!(
            auto_thread_count(host, workload(FfmpegDecoderId::Av1, 3840, 2160), AMPLE_HOST),
            threads(6)
        );
    }

    #[test]
    fn av1_with_unknown_size_gets_the_4k_cap() {
        let unknown = SoftwareDecodeWorkload {
            decoder: FfmpegDecoderId::Av1,
            coded_size: CodedFrameSize::from_optional(Some(3840), None),
        };
        assert_eq!(unknown.coded_size, CodedFrameSize::Unknown);
        assert_eq!(
            auto_thread_count(threads(14), unknown, AMPLE_HOST),
            threads(6)
        );
    }

    #[test]
    fn frame_threaded_codecs_keep_the_host_headroom_rule() {
        for decoder in [
            FfmpegDecoderId::H264,
            FfmpegDecoderId::Hevc,
            FfmpegDecoderId::Vp9,
            FfmpegDecoderId::Vp8,
        ] {
            assert_eq!(
                auto_thread_count(threads(14), workload(decoder, 3840, 2160), AMPLE_HOST),
                threads(14)
            );
            assert_eq!(
                auto_thread_count(threads(30), workload(decoder, 1920, 1080), AMPLE_HOST),
                threads(30)
            );
        }
    }

    #[test]
    fn cap_never_raises_threads_on_a_small_host() {
        assert_eq!(
            auto_thread_count(
                threads(2),
                workload(FfmpegDecoderId::Av1, 3840, 2160),
                AMPLE_HOST
            ),
            threads(2)
        );
        assert_eq!(
            auto_thread_count(
                threads(3),
                workload(FfmpegDecoderId::Av1, 1920, 1080),
                AMPLE_HOST
            ),
            threads(3)
        );
    }

    #[test]
    fn av1_max_frame_delay_depends_on_frame_size() {
        let delay = |width, height| {
            workload(FfmpegDecoderId::Av1, width, height).max_frame_delay(AMPLE_HOST)
        };
        assert_eq!(delay(1280, 720), Some(1));
        assert_eq!(delay(1920, 1080), Some(1));
        assert_eq!(delay(1920, 1088), Some(1));
        assert_eq!(delay(2560, 1440), Some(2));
        assert_eq!(delay(3840, 2160), Some(2));
        let unknown = SoftwareDecodeWorkload {
            decoder: FfmpegDecoderId::Av1,
            coded_size: CodedFrameSize::Unknown,
        };
        assert_eq!(unknown.max_frame_delay(AMPLE_HOST), Some(2));
    }

    #[test]
    fn other_decoders_get_no_frame_delay() {
        for decoder in [
            FfmpegDecoderId::H264,
            FfmpegDecoderId::Hevc,
            FfmpegDecoderId::Vp9,
            FfmpegDecoderId::Vp8,
        ] {
            assert_eq!(
                workload(decoder, 3840, 2160).max_frame_delay(AMPLE_HOST),
                None
            );
        }
    }

    #[test]
    fn open_request_carries_frame_delay_and_resolved_threads() {
        let formats = SoftwarePixelFormatSet::v1_host_planar();
        let request_for = |decoder, width, height| {
            workload(decoder, width, height).apply_to_request(
                FfmpegCodecContextRequest::for_decoder_id(decoder, formats.clone()),
                SoftwareDecodeThreadBudget::auto(),
                AMPLE_HOST,
            )
        };
        let host_headroom = SoftwareDecodeThreadBudget::auto().resolved_thread_count();

        let av1_4k = request_for(FfmpegDecoderId::Av1, 3840, 2160);
        assert_eq!(av1_4k.max_frame_delay(), Some(2));
        assert_eq!(
            av1_4k.software_decode_thread_budget(),
            SoftwareDecodeThreadBudget::fixed(host_headroom.min(AV1_ABOVE_1080P_THREAD_CAP))
        );
        assert_eq!(
            request_for(FfmpegDecoderId::Av1, 1920, 1080).max_frame_delay(),
            Some(1)
        );
        assert_eq!(
            request_for(FfmpegDecoderId::H264, 3840, 2160).max_frame_delay(),
            None
        );
    }

    #[test]
    fn manual_override_is_passed_through_unchanged() {
        let manual = SoftwareDecodeThreadBudget::fixed(threads(12));
        assert_eq!(
            resolve_decode_thread_budget(
                manual,
                workload(FfmpegDecoderId::Av1, 3840, 2160),
                AMPLE_HOST
            ),
            manual
        );
    }

    #[test]
    fn auto_budget_resolves_to_a_fixed_count_within_host_headroom() {
        let host_headroom = SoftwareDecodeThreadBudget::auto().resolved_thread_count();
        let resolved = resolve_decode_thread_budget(
            SoftwareDecodeThreadBudget::auto(),
            workload(FfmpegDecoderId::Av1, 3840, 2160),
            AMPLE_HOST,
        );
        assert_eq!(
            resolved,
            SoftwareDecodeThreadBudget::fixed(host_headroom.min(AV1_ABOVE_1080P_THREAD_CAP))
        );
        let h264 = resolve_decode_thread_budget(
            SoftwareDecodeThreadBudget::auto(),
            workload(FfmpegDecoderId::H264, 3840, 2160),
            AMPLE_HOST,
        );
        assert_eq!(h264, SoftwareDecodeThreadBudget::fixed(host_headroom));
    }

    #[test]
    fn constrained_host_gives_av1_up_to_1080p_two_frames_in_flight() {
        let delay = |width, height, host| {
            workload(FfmpegDecoderId::Av1, width, height).max_frame_delay(host)
        };
        // T410: без второго кадра в полёте AV1 1080p60 декодируется ~29 кадров/с.
        assert_eq!(delay(1920, 1080, T410_HOST), Some(2));
        assert_eq!(delay(1280, 720, T410_HOST), Some(2));
        assert_eq!(delay(3840, 2160, T410_HOST), Some(2));
        // Мощная машина сохраняет правило Session 29 (один кадр до 1080p).
        assert_eq!(delay(1920, 1080, AMPLE_HOST), Some(1));
        // Граница: 5 логических CPU — уже не слабая машина.
        let five_cpus = HostCpuProfile::with_logical_cpus(threads(5));
        assert_eq!(delay(1920, 1080, five_cpus), Some(1));
    }

    #[test]
    fn constrained_host_gives_hevc_every_logical_cpu_plus_two() {
        // «Ядра − 2» на 4 логических CPU = 2 потока; HEVC получает все 4 и ещё 2.
        let headroom = threads(2);
        assert_eq!(
            auto_thread_count(
                headroom,
                workload(FfmpegDecoderId::Hevc, 1920, 1080),
                T410_HOST
            ),
            threads(6)
        );
        let two_cpus = HostCpuProfile::with_logical_cpus(threads(2));
        assert_eq!(
            auto_thread_count(
                headroom,
                workload(FfmpegDecoderId::Hevc, 1920, 1080),
                two_cpus
            ),
            threads(4)
        );
        // На мощной машине HEVC остаётся на «ядра − 2».
        assert_eq!(
            auto_thread_count(
                threads(14),
                workload(FfmpegDecoderId::Hevc, 1920, 1080),
                AMPLE_HOST
            ),
            threads(14)
        );
    }

    #[test]
    fn constrained_host_keeps_h264_vp9_and_av1_on_the_headroom_rule() {
        // H.264/VP9 укладываются в 2 потока на 1080p60, лишние потоки только жгут CPU;
        // AV1 на слабой машине ускоряет задержка кадров, а не потоки.
        for decoder in [
            FfmpegDecoderId::H264,
            FfmpegDecoderId::Vp9,
            FfmpegDecoderId::Vp8,
            FfmpegDecoderId::Av1,
        ] {
            assert_eq!(
                auto_thread_count(threads(2), workload(decoder, 1920, 1080), T410_HOST),
                threads(2),
                "{decoder:?}"
            );
        }
    }

    #[test]
    fn manual_threads_stay_manual_on_a_constrained_host() {
        let manual = SoftwareDecodeThreadBudget::fixed(threads(1));
        assert_eq!(
            resolve_decode_thread_budget(
                manual,
                workload(FfmpegDecoderId::Hevc, 1920, 1080),
                T410_HOST
            ),
            manual
        );
    }

    #[test]
    fn open_request_on_a_constrained_host_carries_the_weak_machine_rules() {
        let formats = SoftwarePixelFormatSet::v1_host_planar();
        let av1 = workload(FfmpegDecoderId::Av1, 1920, 1080).apply_to_request(
            FfmpegCodecContextRequest::for_decoder_id(FfmpegDecoderId::Av1, formats.clone()),
            SoftwareDecodeThreadBudget::auto(),
            T410_HOST,
        );
        assert_eq!(av1.max_frame_delay(), Some(2));
        let hevc = workload(FfmpegDecoderId::Hevc, 1920, 1080).apply_to_request(
            FfmpegCodecContextRequest::for_decoder_id(FfmpegDecoderId::Hevc, formats),
            SoftwareDecodeThreadBudget::auto(),
            T410_HOST,
        );
        let host_headroom = SoftwareDecodeThreadBudget::auto().resolved_thread_count();
        assert_eq!(
            hevc.software_decode_thread_budget(),
            SoftwareDecodeThreadBudget::fixed(threads(6).max(host_headroom))
        );
        assert_eq!(hevc.max_frame_delay(), None);
    }
}
