//! Параметрическое битовое распределение (A/52 §7.2.2, шаги 1–7).
//!
//! Алгоритм обязан давать бит-в-бит одинаковый результат в кодере и декодере
//! (§7.2.1), поэтому он записан в целочисленной арифметике ровно по
//! псевдокоду спецификации («вертикальное исполнение»). Результат — массив
//! `bap[]` (указатель квантователя каждого бина), по которому затем читаются
//! мантиссы.

use super::COEFFICIENTS_PER_BLOCK;
use crate::tables::bit_allocation::{
    BAND_COUNT, BAND_SIZE, BAND_START, BAP_TABLE, BIN_TO_BAND, DB_PER_BIT, FAST_DECAY, FAST_GAIN,
    FLOOR, HEARING_THRESHOLD, LOG_ADDITION, SLOW_DECAY, SLOW_GAIN, delta_bit_allocation_adjustment,
};
use crate::tables::eac3::aht::HIGH_EFFICIENCY_BAP_TABLE;
use crate::tables::frame_size::SampleRateCode;

/// Наибольшее число сегментов delta bit allocation (`deltnseg + 1`, §5.4.3.54).
pub(crate) const MAX_DELTA_SEGMENTS: usize = 8;

/// PSD нулевой экспоненты: `psd = 3072 − (exp << 7)` (§7.2.2.2).
const PSD_OF_ZERO_EXPONENT: i32 = 3072;
/// Маска после вычитания offset-а обрезается до шага 32 (§7.2.2.7: `&= 0x1fe0`).
const MASK_QUANTIZATION: i32 = 0x1fe0;
/// Наибольший адрес `baptab[]`.
const MAX_BAP_ADDRESS: i32 = 63;
/// Excitation fast-leak начинается до 22-й полосы особым образом (§7.2.2.4).
const LOW_FREQUENCY_BAND_LIMIT: usize = 22;
/// Полосы 0–6 обрабатываются отдельно для fbw/LFE (§7.2.2.4).
const FIRST_REGULAR_BAND: usize = 7;
/// `calc_lowcomp`: границы диапазонов полос.
const LOWCOMP_FIRST_LIMIT: usize = 7;
const LOWCOMP_SECOND_LIMIT: usize = 20;

/// Общие для блока параметры из `baie` (Таблицы 7.6–7.10), уже раскрытые.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BitAllocationParameters {
    slow_decay: i32,
    fast_decay: i32,
    slow_gain: i32,
    db_per_bit_knee: i32,
    floor: i32,
}

impl BitAllocationParameters {
    /// Раскрывает коды `sdcycod`, `fdcycod`, `sgaincod`, `dbpbcod`, `floorcod`.
    /// Ширина полей (2/2/2/2/3 бита) гарантирует попадание в таблицы; маска
    /// делает это явным.
    pub(crate) fn from_codes(
        slow_decay_code: u8,
        fast_decay_code: u8,
        slow_gain_code: u8,
        db_per_bit_code: u8,
        floor_code: u8,
    ) -> Self {
        Self {
            slow_decay: SLOW_DECAY[usize::from(slow_decay_code & 0x3)],
            fast_decay: FAST_DECAY[usize::from(fast_decay_code & 0x3)],
            slow_gain: SLOW_GAIN[usize::from(slow_gain_code & 0x3)],
            db_per_bit_knee: DB_PER_BIT[usize::from(db_per_bit_code & 0x3)],
            floor: FLOOR[usize::from(floor_code & 0x7)],
        }
    }
}

/// Fast gain по `fgaincod` (Таблица 7.11).
pub(crate) fn fast_gain(code: u8) -> i32 {
    FAST_GAIN[usize::from(code & 0x7)]
}

/// `snroffset = (((csnroffst − 15) << 4) + fsnroffst) << 2` (§7.2.2.1).
pub(crate) fn snr_offset(coarse: u8, fine: u8) -> i32 {
    (((i32::from(coarse) - 15) << 4) + i32::from(fine)) << 2
}

/// Один сегмент delta bit allocation (§5.4.3.51–5.4.3.53).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DeltaSegment {
    /// `deltoffst`: смещение от конца прошлого сегмента, в полосах.
    pub(crate) offset: u8,
    /// `deltlen`: длина сегмента, в полосах.
    pub(crate) length: u8,
    /// `deltba`: код поправки (Таблица 5.17).
    pub(crate) adjustment_code: u8,
}

/// Набор сегментов одного канала (не более 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DeltaSegments {
    segments: [DeltaSegment; MAX_DELTA_SEGMENTS],
    count: usize,
}

impl DeltaSegments {
    /// Добавляет сегмент; лишние (больше 8) отбрасывает — их не бывает,
    /// потому что `deltnseg` 3-битный.
    pub(crate) fn push(&mut self, segment: DeltaSegment) {
        if let Some(slot) = self.segments.get_mut(self.count) {
            *slot = segment;
            self.count += 1;
        }
    }

    fn iter(&self) -> impl Iterator<Item = &DeltaSegment> {
        self.segments.iter().take(self.count)
    }
}

/// Начальные значения leak-ов coupling канала (§7.2.2.1: `(cplfleak << 8) + 768`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LeakInitialization {
    fast: i32,
    slow: i32,
}

impl LeakInitialization {
    /// Раскрывает `cplfleak`, `cplsleak`.
    pub(crate) fn from_codes(fast_leak_code: u8, slow_leak_code: u8) -> Self {
        /// Смещение leak-инициализации из §7.2.2.1.
        const LEAK_BIAS: i32 = 768;
        Self {
            fast: (i32::from(fast_leak_code) << 8) + LEAK_BIAS,
            slow: (i32::from(slow_leak_code) << 8) + LEAK_BIAS,
        }
    }
}

/// Какой канал распределяется: от этого зависит начало excitation (§7.2.2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AllocationChannelKind {
    /// Полнополосный или LFE канал: начинается с полосы 0, есть `lowcomp`.
    StartsAtDc,
    /// Coupling канал: начинается с `masktab[cplstrtmant]`, leak-и заданы потоком.
    Coupling(LeakInitialization),
}

/// Всё, что нужно для распределения одного набора экспонент.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ChannelAllocationInput<'a> {
    /// Абсолютные экспоненты блока, индекс — номер бина.
    pub(crate) exponents: &'a [u8; COEFFICIENTS_PER_BLOCK],
    /// Первый бин (`strtmant`).
    pub(crate) start: usize,
    /// Бин за последним (`endmant`), не больше 253.
    pub(crate) end: usize,
    /// `fgain` по `fgaincod`.
    pub(crate) fast_gain: i32,
    /// `snroffset`.
    pub(crate) snr_offset: i32,
    /// Тип канала.
    pub(crate) kind: AllocationChannelKind,
    /// Сегменты delta bit allocation, если поправка активна.
    pub(crate) deltas: Option<&'a DeltaSegments>,
}

/// Сегмент delta bit allocation вышел за 50 полос (§7.10.2, условия 15/16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeltaBeyondBands;

/// Какая таблица превращает адрес «PSD − маска» в указатель квантователя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AllocationPointerTable {
    /// `baptab[]` (Таблица 7.16) — обычные указатели bap 0–15.
    Conventional,
    /// `hebaptab[]` (Annex E Таблица E3.1) — указатели hebap 0–19 каналов с AHT
    /// (Annex E §3.4.3.1: все шаги до финального поиска те же).
    HighEfficiency,
}

impl AllocationPointerTable {
    const fn lookup(self) -> &'static [u8; 64] {
        match self {
            Self::Conventional => &BAP_TABLE,
            Self::HighEfficiency => &HIGH_EFFICIENCY_BAP_TABLE,
        }
    }
}

/// Считает `bap[start..end]` (остальные бины не трогает).
pub(crate) fn compute_bit_allocation(
    parameters: &BitAllocationParameters,
    sample_rate: SampleRateCode,
    input: &ChannelAllocationInput<'_>,
    bap: &mut [u8; COEFFICIENTS_PER_BLOCK],
) -> Result<(), DeltaBeyondBands> {
    compute_bit_allocation_with_table(
        parameters,
        sample_rate,
        input,
        AllocationPointerTable::Conventional,
        bap,
    )
}

/// То же распределение с выбором таблицы указателей (`bap` или `hebap`).
pub(crate) fn compute_bit_allocation_with_table(
    parameters: &BitAllocationParameters,
    sample_rate: SampleRateCode,
    input: &ChannelAllocationInput<'_>,
    table: AllocationPointerTable,
    bap: &mut [u8; COEFFICIENTS_PER_BLOCK],
) -> Result<(), DeltaBeyondBands> {
    let end = input.end.min(COEFFICIENTS_PER_BLOCK);
    let start = input.start.min(end);
    if start == end {
        return Ok(());
    }
    let psd = exponents_to_psd(input.exponents, start, end);
    let band_psd = integrate_psd(&psd, start, end);
    let band_start = usize::from(BIN_TO_BAND[start]);
    let band_end = usize::from(BIN_TO_BAND[end - 1]) + 1;
    let mut excitation = compute_excitation(parameters, input, &band_psd, band_start, band_end);
    let mut mask = compute_masking_curve(
        parameters,
        sample_rate,
        &band_psd,
        &mut excitation,
        band_start,
        band_end,
    );
    if let Some(deltas) = input.deltas {
        apply_delta_bit_allocation(deltas, &mut mask)?;
    }
    assign_bit_allocation_pointers(
        parameters,
        input.snr_offset,
        &psd,
        &mut mask,
        start..end,
        table.lookup(),
        bap,
    );
    Ok(())
}

/// Шаг 2 (§7.2.2.2): экспоненты → 13-битная логарифмическая PSD.
fn exponents_to_psd(
    exponents: &[u8; COEFFICIENTS_PER_BLOCK],
    start: usize,
    end: usize,
) -> [i32; COEFFICIENTS_PER_BLOCK] {
    let mut psd = [0i32; COEFFICIENTS_PER_BLOCK];
    for bin in start..end {
        psd[bin] = PSD_OF_ZERO_EXPONENT - (i32::from(exponents[bin]) << 7);
    }
    psd
}

/// `logadd(a, b)` из §7.2.2.3.
fn log_add(first: i32, second: i32) -> i32 {
    let difference = first - second;
    let address = ((difference.abs() >> 1).min(255)) as usize;
    let addition = LOG_ADDITION[address];
    if difference >= 0 {
        first + addition
    } else {
        second + addition
    }
}

/// Шаг 3 (§7.2.2.3): лог-сложение PSD внутри каждой полосы 1/6 октавы.
fn integrate_psd(
    psd: &[i32; COEFFICIENTS_PER_BLOCK],
    start: usize,
    end: usize,
) -> [i32; BAND_COUNT] {
    let mut band_psd = [0i32; BAND_COUNT];
    let mut bin = start;
    let mut band = usize::from(BIN_TO_BAND[start]);
    while bin < end && band < BAND_COUNT {
        let last_bin = (usize::from(BAND_START[band]) + usize::from(BAND_SIZE[band])).min(end);
        let mut accumulated = psd[bin];
        bin += 1;
        while bin < last_bin {
            accumulated = log_add(accumulated, psd[bin]);
            bin += 1;
        }
        band_psd[band] = accumulated;
        band += 1;
    }
    band_psd
}

/// `calc_lowcomp(a, b0, b1, bin)` из §7.2.2.4.
///
/// В спецификации после `if ((b0 + 256) == b1)` для `bin < 7` стоит лишняя
/// `;` — это опечатка (иначе `a = 384` выполнялось бы всегда, а `else if`
/// не компилировался бы); читаем как в двух соседних ветках.
fn low_compensation(previous: i32, current_band: i32, next_band: i32, band: usize) -> i32 {
    if band < LOWCOMP_FIRST_LIMIT {
        if current_band + 256 == next_band {
            384
        } else if current_band > next_band {
            (previous - 64).max(0)
        } else {
            previous
        }
    } else if band < LOWCOMP_SECOND_LIMIT {
        if current_band + 256 == next_band {
            320
        } else if current_band > next_band {
            (previous - 64).max(0)
        } else {
            previous
        }
    } else {
        (previous - 128).max(0)
    }
}

/// Состояние excitation-проходa: fast/slow leak и низкочастотная компенсация.
struct ExcitationState {
    fast_leak: i32,
    slow_leak: i32,
    low_compensation: i32,
}

/// Шаг 4 (§7.2.2.4): функция возбуждения.
fn compute_excitation(
    parameters: &BitAllocationParameters,
    input: &ChannelAllocationInput<'_>,
    band_psd: &[i32; BAND_COUNT],
    band_start: usize,
    band_end: usize,
) -> [i32; BAND_COUNT] {
    let mut excitation = [0i32; BAND_COUNT];
    let band_end = band_end.min(BAND_COUNT);
    let fast_gain = input.fast_gain;
    let next_band_psd = |band: usize| band_psd.get(band + 1).copied().unwrap_or_default();
    // Для LFE (bndend == 7) у последней полосы 6 нет соседа справа (§7.2.2.4).
    let has_right_neighbour = |band: usize| band_end != FIRST_REGULAR_BAND || band != 6;
    let (begin, mut state) = match input.kind {
        AllocationChannelKind::Coupling(leaks) => (
            band_start,
            ExcitationState {
                fast_leak: leaks.fast,
                slow_leak: leaks.slow,
                low_compensation: 0,
            },
        ),
        AllocationChannelKind::StartsAtDc => {
            let mut state = ExcitationState {
                fast_leak: 0,
                slow_leak: 0,
                low_compensation: 0,
            };
            for band in 0..2 {
                state.low_compensation = low_compensation(
                    state.low_compensation,
                    band_psd[band],
                    next_band_psd(band),
                    band,
                );
                excitation[band] = band_psd[band] - fast_gain - state.low_compensation;
            }
            let mut begin = FIRST_REGULAR_BAND;
            for band in 2..FIRST_REGULAR_BAND.min(band_end) {
                if has_right_neighbour(band) {
                    state.low_compensation = low_compensation(
                        state.low_compensation,
                        band_psd[band],
                        next_band_psd(band),
                        band,
                    );
                }
                state.fast_leak = band_psd[band] - fast_gain;
                state.slow_leak = band_psd[band] - parameters.slow_gain;
                excitation[band] = state.fast_leak - state.low_compensation;
                if has_right_neighbour(band) && band_psd[band] <= next_band_psd(band) {
                    begin = band + 1;
                    break;
                }
            }
            for band in begin..band_end.min(LOW_FREQUENCY_BAND_LIMIT) {
                if has_right_neighbour(band) {
                    state.low_compensation = low_compensation(
                        state.low_compensation,
                        band_psd[band],
                        next_band_psd(band),
                        band,
                    );
                }
                state.fast_leak =
                    (state.fast_leak - parameters.fast_decay).max(band_psd[band] - fast_gain);
                state.slow_leak = (state.slow_leak - parameters.slow_decay)
                    .max(band_psd[band] - parameters.slow_gain);
                excitation[band] = (state.fast_leak - state.low_compensation).max(state.slow_leak);
            }
            (LOW_FREQUENCY_BAND_LIMIT, state)
        }
    };
    for band in begin..band_end {
        state.fast_leak = (state.fast_leak - parameters.fast_decay).max(band_psd[band] - fast_gain);
        state.slow_leak =
            (state.slow_leak - parameters.slow_decay).max(band_psd[band] - parameters.slow_gain);
        excitation[band] = state.fast_leak.max(state.slow_leak);
    }
    excitation
}

/// Шаг 5 (§7.2.2.5): кривая маскирования с порогом слышимости.
fn compute_masking_curve(
    parameters: &BitAllocationParameters,
    sample_rate: SampleRateCode,
    band_psd: &[i32; BAND_COUNT],
    excitation: &mut [i32; BAND_COUNT],
    band_start: usize,
    band_end: usize,
) -> [i32; BAND_COUNT] {
    let mut mask = [0i32; BAND_COUNT];
    let hearing_threshold = &HEARING_THRESHOLD[sample_rate.table_index()];
    for band in band_start..band_end.min(BAND_COUNT) {
        if band_psd[band] < parameters.db_per_bit_knee {
            excitation[band] += (parameters.db_per_bit_knee - band_psd[band]) >> 2;
        }
        mask[band] = excitation[band].max(hearing_threshold[band]);
    }
    mask
}

/// Шаг 6 (§7.2.2.6): поправки маски, переданные кодером.
fn apply_delta_bit_allocation(
    deltas: &DeltaSegments,
    mask: &mut [i32; BAND_COUNT],
) -> Result<(), DeltaBeyondBands> {
    let mut band = 0usize;
    for segment in deltas.iter() {
        band += usize::from(segment.offset);
        let adjustment = delta_bit_allocation_adjustment(segment.adjustment_code);
        for _ in 0..segment.length {
            let slot = mask.get_mut(band).ok_or(DeltaBeyondBands)?;
            *slot += adjustment;
            band += 1;
        }
    }
    Ok(())
}

/// Шаг 7 (§7.2.2.7): маска со SNR offset-ом → указатели `bap`.
fn assign_bit_allocation_pointers(
    parameters: &BitAllocationParameters,
    snr_offset: i32,
    psd: &[i32; COEFFICIENTS_PER_BLOCK],
    mask: &mut [i32; BAND_COUNT],
    bins: std::ops::Range<usize>,
    pointer_table: &[u8; 64],
    bap: &mut [u8; COEFFICIENTS_PER_BLOCK],
) {
    let (start, end) = (bins.start, bins.end);
    let mut bin = start;
    let mut band = usize::from(BIN_TO_BAND[start]);
    while bin < end && band < BAND_COUNT {
        let last_bin = (usize::from(BAND_START[band]) + usize::from(BAND_SIZE[band])).min(end);
        let mut band_mask = mask[band] - snr_offset - parameters.floor;
        band_mask = band_mask.max(0) & MASK_QUANTIZATION;
        band_mask += parameters.floor;
        mask[band] = band_mask;
        while bin < last_bin {
            let address = ((psd[bin] - band_mask) >> 5).clamp(0, MAX_BAP_ADDRESS);
            bap[bin] = pointer_table[address as usize];
            bin += 1;
        }
        band += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AllocationChannelKind, BitAllocationParameters, ChannelAllocationInput, DeltaSegment,
        DeltaSegments, LeakInitialization, compute_bit_allocation, fast_gain, log_add, snr_offset,
    };
    use crate::tables::frame_size::SampleRateCode;

    fn default_parameters() -> BitAllocationParameters {
        // Типичные коды кодера: sdcycod 2, fdcycod 1, sgaincod 1, dbpbcod 2, floorcod 7.
        BitAllocationParameters::from_codes(2, 1, 1, 2, 7)
    }

    fn allocate(
        exponents: &[u8; 256],
        end: usize,
        snr: i32,
        deltas: Option<&DeltaSegments>,
    ) -> [u8; 256] {
        let mut bap = [0u8; 256];
        let input = ChannelAllocationInput {
            exponents,
            start: 0,
            end,
            fast_gain: fast_gain(4),
            snr_offset: snr,
            kind: AllocationChannelKind::StartsAtDc,
            deltas,
        };
        compute_bit_allocation(
            &default_parameters(),
            SampleRateCode::Rate48000,
            &input,
            &mut bap,
        )
        .unwrap();
        bap
    }

    #[test]
    fn log_add_uses_larger_operand_plus_table() {
        assert_eq!(log_add(1000, 1000), 1000 + 0x40);
        assert_eq!(log_add(1000, 0), 1000);
        assert_eq!(log_add(0, 20), 20 + 0x36); // |−20| >> 1 = 10 → latab[10]
    }

    #[test]
    fn snr_offset_formula() {
        assert_eq!(snr_offset(15, 0), 0);
        assert_eq!(snr_offset(0, 0), -960);
        assert_eq!(snr_offset(63, 15), (48 * 16 + 15) * 4);
    }

    #[test]
    fn louder_spectrum_and_higher_snr_get_more_bits() {
        // Инвариант §7.2: меньшая экспонента (громче) или больший SNR offset
        // никогда не уменьшают число бит; за end ничего не пишется.
        let quiet = [12u8; 256];
        let loud = [2u8; 256];
        let base = allocate(&quiet, 100, snr_offset(15, 0), None);
        let louder = allocate(&loud, 100, snr_offset(15, 0), None);
        let richer = allocate(&quiet, 100, snr_offset(30, 0), None);
        for bin in 0..100 {
            assert!(louder[bin] >= base[bin], "bin {bin}");
            assert!(richer[bin] >= base[bin], "bin {bin}");
        }
        assert!(
            louder[..100].iter().map(|&bap| u32::from(bap)).sum::<u32>()
                > base[..100].iter().map(|&bap| u32::from(bap)).sum::<u32>()
        );
        assert!(base[100..].iter().all(|&bap| bap == 0));
    }

    #[test]
    fn negative_delta_lowers_mask_and_adds_bits_only_in_its_bands() {
        let exponents = [8u8; 256];
        let plain = allocate(&exponents, 253, snr_offset(10, 0), None);
        let mut deltas = DeltaSegments::default();
        // Полосы 40–41 (бины 79–96): −24 дБ.
        deltas.push(DeltaSegment {
            offset: 40,
            length: 2,
            adjustment_code: 0,
        });
        let adjusted = allocate(&exponents, 253, snr_offset(10, 0), Some(&deltas));
        for bin in 0..253 {
            if (79..97).contains(&bin) {
                assert!(adjusted[bin] > plain[bin], "bin {bin}");
            } else {
                assert_eq!(adjusted[bin], plain[bin], "bin {bin}");
            }
        }
    }

    #[test]
    fn delta_beyond_fifty_bands_is_rejected() {
        let exponents = [8u8; 256];
        let mut deltas = DeltaSegments::default();
        deltas.push(DeltaSegment {
            offset: 31,
            length: 15,
            adjustment_code: 7,
        });
        deltas.push(DeltaSegment {
            offset: 31,
            length: 1,
            adjustment_code: 7,
        });
        let mut bap = [0u8; 256];
        let input = ChannelAllocationInput {
            exponents: &exponents,
            start: 0,
            end: 253,
            fast_gain: fast_gain(0),
            snr_offset: 0,
            kind: AllocationChannelKind::StartsAtDc,
            deltas: Some(&deltas),
        };
        assert!(
            compute_bit_allocation(
                &default_parameters(),
                SampleRateCode::Rate44100,
                &input,
                &mut bap
            )
            .is_err()
        );
    }

    #[test]
    fn coupling_channel_writes_only_its_range() {
        let exponents = [3u8; 256];
        let mut bap = [99u8; 256];
        let input = ChannelAllocationInput {
            exponents: &exponents,
            start: 37 + 12 * 4,
            end: 37 + 12 * 10,
            fast_gain: fast_gain(3),
            snr_offset: snr_offset(20, 4),
            kind: AllocationChannelKind::Coupling(LeakInitialization::from_codes(0, 0)),
            deltas: None,
        };
        compute_bit_allocation(
            &default_parameters(),
            SampleRateCode::Rate32000,
            &input,
            &mut bap,
        )
        .unwrap();
        assert!(bap[..85].iter().all(|&value| value == 99));
        assert!(bap[157..].iter().all(|&value| value == 99));
        assert!(bap[85..157].iter().all(|&value| value <= 15));
        assert!(bap[85..157].iter().any(|&value| value > 0));
    }
}
