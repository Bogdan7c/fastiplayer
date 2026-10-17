//! Adapter не искажает звук: сравнение с прямым выходом `ac3-decoder` и с ffmpeg.
//!
//! 1. Бит-в-бит: каждый canonical lane adapter-а равен каналу прямого
//!    `Ac3Decoder` с теми же настройками; ожидаемая перестановка записана здесь
//!    вручную по Т. 5.8 (а не взята из кода adapter-а), поэтому ошибка порядка
//!    каналов или масштаба в adapter-е не может совпасть с ожиданием.
//! 2. Против эталона ffmpeg (выдержка `5p1-48k-640k-annexd.ffmpeg-f32le`, порядок
//!    ffmpeg FL FR FC LFE SL SR = canonical порядок adapter-а): LFE без dither-а
//!    совпадает до округления f32, остальные каналы — до уровня dither-а/сетки ffmpeg.

use ac3_decoder::Ac3Decoder;
use audio_core::AudioDecoder;

use super::super::{Ac3CodecIdentity, production_decoder_config};
use super::{STEREO_AC3, SURROUND_AC3, SURROUND_EAC3, concrete_decoder, decode_packet, packets};

/// Путь к фикстуре собственного декодера.
macro_rules! decoder_fixture {
    ($name:literal) => {
        include_bytes!(concat!("../../../../../ac3-decoder/tests/fixtures/", $name))
    };
}

/// Поток и ожидаемая перестановка: `expected_sources[lane]` — канал декодера.
struct DifferentialCase {
    /// Имя для сообщений.
    name: &'static str,
    /// Elementary stream.
    stream: &'static [u8],
    /// Identity дорожки.
    identity: Ac3CodecIdentity,
    /// Канал декодера для каждого canonical lane (вручную по Т. 5.8).
    expected_sources: &'static [usize],
}

/// Раскладки AC-3: mono, 2/0, 2/1, 3/0+LFE, 2/2, 3/1+LFE, 3/2+LFE.
const AC3_CASES: [DifferentialCase; 7] = [
    DifferentialCase {
        name: "mono C",
        stream: decoder_fixture!("mono-48k-96k.ac3"),
        identity: Ac3CodecIdentity::Ac3,
        expected_sources: &[0],
    },
    DifferentialCase {
        name: "stereo L R",
        stream: STEREO_AC3,
        identity: Ac3CodecIdentity::Ac3,
        expected_sources: &[0, 1],
    },
    DifferentialCase {
        // L, R, S → FL, FR, RearCenter.
        name: "2/1 L R S",
        stream: decoder_fixture!("2f1r-32k-192k.ac3"),
        identity: Ac3CodecIdentity::Ac3,
        expected_sources: &[0, 1, 2],
    },
    DifferentialCase {
        // L, C, R, LFE → FL, FR, FC, LFE.
        name: "3/0+LFE L C R LFE",
        stream: decoder_fixture!("3f-lfe-32k-256k.ac3"),
        identity: Ac3CodecIdentity::Ac3,
        expected_sources: &[0, 2, 1, 3],
    },
    DifferentialCase {
        // L, R, Ls, Rs → FL, FR, SL, SR.
        name: "2/2 L R Ls Rs",
        stream: decoder_fixture!("2f2r-48k-320k.ac3"),
        identity: Ac3CodecIdentity::Ac3,
        expected_sources: &[0, 1, 2, 3],
    },
    DifferentialCase {
        // L, C, R, S, LFE → FL, FR, FC, LFE, RearCenter.
        name: "3/1+LFE L C R S LFE",
        stream: decoder_fixture!("3f1r-lfe-48k-384k.ac3"),
        identity: Ac3CodecIdentity::Ac3,
        expected_sources: &[0, 2, 1, 4, 3],
    },
    DifferentialCase {
        // L, C, R, Ls, Rs, LFE → FL, FR, FC, LFE, SL, SR.
        name: "3/2+LFE L C R Ls Rs LFE",
        stream: SURROUND_AC3,
        identity: Ac3CodecIdentity::Ac3,
        expected_sources: &[0, 2, 1, 5, 3, 4],
    },
];

/// E-AC-3 5.1 (зависит от синтеза PCM E-AC-3 в `ac3-decoder`).
const EAC3_SURROUND_CASE: DifferentialCase = DifferentialCase {
    name: "E-AC-3 3/2+LFE",
    stream: SURROUND_EAC3,
    identity: Ac3CodecIdentity::Eac3,
    expected_sources: &[0, 2, 1, 5, 3, 4],
};

/// Каждый packet через adapter совпадает бит-в-бит с прямым декодером.
fn assert_adapter_is_transparent(case: &DifferentialCase) {
    let mut adapter = concrete_decoder(case.identity, 2);
    let mut direct = Ac3Decoder::new(production_decoder_config());

    for (packet_index, packet_bytes) in packets(case.stream).into_iter().enumerate() {
        let adapted = decode_packet(&mut adapter, packet_bytes);
        let reference = direct.decode_access_unit(packet_bytes).unwrap();
        let spec = adapter.decoded_output_spec().unwrap();
        let channels = case.expected_sources.len();

        assert_eq!(
            spec.sample_rate,
            reference.sample_rate_hz(),
            "{}",
            case.name
        );
        assert_eq!(spec.channels() as usize, channels, "{}", case.name);
        assert_eq!(
            adapted.len(),
            reference.samples_per_channel() * channels,
            "{}",
            case.name
        );
        for (lane, source) in case.expected_sources.iter().enumerate() {
            let reference_channel = reference.channel_samples(*source).unwrap();
            let adapted_lane: Vec<f32> = adapted
                .iter()
                .skip(lane)
                .step_by(channels)
                .copied()
                .collect();
            assert!(
                adapted_lane.iter().zip(reference_channel).all(
                    |(adapted_sample, reference_sample)| {
                        adapted_sample.to_bits() == reference_sample.to_bits()
                    }
                ),
                "{} packet {packet_index}: lane {lane} differs from decoder channel {source}",
                case.name
            );
        }
    }
}

#[test]
fn adapter_output_equals_direct_decoder_channels_for_every_ac3_layout() {
    for case in &AC3_CASES {
        assert_adapter_is_transparent(case);
    }
}

#[test]
fn adapter_output_equals_direct_decoder_channels_for_eac3_5_1() {
    assert_adapter_is_transparent(&EAC3_SURROUND_CASE);
}

/// 5.1 Annex D 640k: поток и выдержка ffmpeg кадра 2 (`reference ... 2 3` в generate.sh).
const ANNEX_D_SURROUND: &[u8] = decoder_fixture!("5p1-48k-640k-annexd.ac3");
/// ffmpeg n9.0.2, f32le interleaved FL FR FC LFE SL SR, кадр 2.
const ANNEX_D_FFMPEG_FRAME_2: &[u8] = decoder_fixture!("5p1-48k-640k-annexd.ffmpeg-f32le");
/// Номер кадра выдержки.
const REFERENCE_FRAME: usize = 2;
/// Lane LFE в canonical 5.1.
const LFE_LANE: usize = 3;
/// LFE без dither-а: совпадение до округления f32 (как в тестах `ac3-decoder`).
const LFE_PSNR_DB: f64 = 120.0;
/// Остальные lanes: полный сигнал против ffmpeg. Остаток задаёт dither кодов
/// bap 0 (у ffmpeg свой генератор шума, A/52 §7.3.4 допускает любой) и сетка
/// фиксированной точки ffmpeg — на этом кадре 31–36 дБ. Точное разделение
/// dither-а и сигнала делают тесты `ac3-decoder` (`reference_comparison`);
/// здесь важно, что порядок и масштаб lanes те же: при ошибке масштаба ×2
/// SNR упал бы до ~0 дБ, при перепутанных каналах — ниже нуля.
const FULL_BAND_SNR_DB: f64 = 25.0;
/// Сдвинутый на один lane выход обязан расходиться с эталоном (SNR ниже нуля).
const WRONG_ORDER_MAX_SNR_DB: f64 = 0.0;

/// Отношение «энергия эталона / энергия остатка» в дБ для одного lane.
fn lane_snr_db(adapted: &[f32], reference: &[f32], channels: usize, lane: usize) -> f64 {
    let (signal, error) = adapted
        .iter()
        .zip(reference)
        .skip(lane)
        .step_by(channels)
        .fold((0.0_f64, 0.0_f64), |(signal, error), (ours, theirs)| {
            let difference = f64::from(*ours) - f64::from(*theirs);
            (
                signal + f64::from(*theirs).powi(2),
                error + difference.powi(2),
            )
        });
    10.0 * (signal / error.max(f64::MIN_POSITIVE)).log10()
}

/// PSNR lane-а относительно полной шкалы 1,0.
fn lane_psnr_db(adapted: &[f32], reference: &[f32], channels: usize, lane: usize) -> f64 {
    let (count, error) = adapted
        .iter()
        .zip(reference)
        .skip(lane)
        .step_by(channels)
        .fold((0_usize, 0.0_f64), |(count, error), (ours, theirs)| {
            (
                count + 1,
                error + (f64::from(*ours) - f64::from(*theirs)).powi(2),
            )
        });
    10.0 * (count as f64 / error.max(f64::MIN_POSITIVE)).log10()
}

#[test]
fn adapter_5_1_frame_matches_ffmpeg_reference_in_canonical_order() {
    let mut adapter = concrete_decoder(Ac3CodecIdentity::Ac3, 6);
    let packet_list = packets(ANNEX_D_SURROUND);
    let mut adapted = Vec::new();
    for packet_bytes in &packet_list[..=REFERENCE_FRAME] {
        adapted = decode_packet(&mut adapter, packet_bytes);
    }
    let (sample_words, tail) = ANNEX_D_FFMPEG_FRAME_2.as_chunks::<4>();
    assert!(tail.is_empty(), "reference must hold whole f32 samples");
    let reference: Vec<f32> = sample_words
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
        .collect();
    let channels = 6;
    assert_eq!(adapted.len(), reference.len());

    let lfe_psnr = lane_psnr_db(&adapted, &reference, channels, LFE_LANE);
    assert!(lfe_psnr >= LFE_PSNR_DB, "LFE PSNR {lfe_psnr:.1} dB");
    for lane in (0..channels).filter(|lane| *lane != LFE_LANE) {
        let snr = lane_snr_db(&adapted, &reference, channels, lane);
        assert!(
            snr >= FULL_BAND_SNR_DB,
            "lane {lane} SNR {snr:.1} dB vs ffmpeg"
        );
        // Тот же эталонный lane против заведомо неправильного порядка каналов.
        let wrong_order_snr = lane_snr_db(
            &rotate_lanes(&adapted, channels),
            &reference,
            channels,
            lane,
        );
        assert!(
            wrong_order_snr < WRONG_ORDER_MAX_SNR_DB,
            "lane {lane} matches a neighbour channel too ({wrong_order_snr:.1} dB)"
        );
    }
}

/// Сдвигает lanes на один (lane i ← lane i+1): заведомо неправильный порядок.
fn rotate_lanes(interleaved: &[f32], channels: usize) -> Vec<f32> {
    interleaved
        .chunks_exact(channels)
        .flat_map(|frame| (0..channels).map(move |lane| frame[(lane + 1) % channels]))
        .collect()
}
