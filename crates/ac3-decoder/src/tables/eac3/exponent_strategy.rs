//! Таблица E2.10 «Frame Exponent Strategy Combinations» (A/52 Annex E §2.3.2.12;
//! ETSI TS 102 366 v1.4.1 Таблица E.1.9).
//!
//! При `expstre = 0` поток передаёт одну 5-битную комбинацию на кадр
//! (`frmcplexpstr`, `frmchexpstr[ch]`), а стратегии экспонент всех шести блоков
//! берутся из этой таблицы. Строка сверена с текстом ATSC и ETSI скриптом
//! `tools/extract_eac3_tables.py` (печать `E2.10`).

use crate::spectral::exponents::ExponentStrategy;

/// Сокращения из текста таблицы: `R` — reuse.
const R: ExponentStrategy = ExponentStrategy::Reuse;
const D15: ExponentStrategy = ExponentStrategy::D15;
const D25: ExponentStrategy = ExponentStrategy::D25;
const D45: ExponentStrategy = ExponentStrategy::D45;

/// Блоков в кадре, для которого определена таблица (`numblkscod = 3`).
pub(crate) const FRAME_STRATEGY_BLOCKS: usize = 6;

/// Таблица E2.10: строка — код `frmcplexpstr`/`frmchexpstr`, столбец — блок.
const FRAME_EXPONENT_STRATEGIES: [[ExponentStrategy; FRAME_STRATEGY_BLOCKS]; 32] = [
    [D15, R, R, R, R, R],           // 0
    [D15, R, R, R, R, D45],         // 1
    [D15, R, R, R, D25, R],         // 2
    [D15, R, R, R, D45, D45],       // 3
    [D25, R, R, D25, R, R],         // 4
    [D25, R, R, D25, R, D45],       // 5
    [D25, R, R, D45, D25, R],       // 6
    [D25, R, R, D45, D45, D45],     // 7
    [D25, R, D15, R, R, R],         // 8
    [D25, R, D25, R, R, D45],       // 9
    [D25, R, D25, R, D25, R],       // 10
    [D25, R, D25, R, D45, D45],     // 11
    [D25, R, D45, D25, R, R],       // 12
    [D25, R, D45, D25, R, D45],     // 13
    [D25, R, D45, D45, D25, R],     // 14
    [D25, R, D45, D45, D45, D45],   // 15
    [D45, D15, R, R, R, R],         // 16
    [D45, D15, R, R, R, D45],       // 17
    [D45, D25, R, R, D25, R],       // 18
    [D45, D25, R, R, D45, D45],     // 19
    [D45, D25, R, D25, R, R],       // 20
    [D45, D25, R, D25, R, D45],     // 21
    [D45, D25, R, D45, D25, R],     // 22
    [D45, D25, R, D45, D45, D45],   // 23
    [D45, D45, D15, R, R, R],       // 24
    [D45, D45, D25, R, R, D45],     // 25
    [D45, D45, D25, R, D25, R],     // 26
    [D45, D45, D25, R, D45, D45],   // 27
    [D45, D45, D45, D25, R, R],     // 28
    [D45, D45, D45, D25, R, D45],   // 29
    [D45, D45, D45, D45, D25, R],   // 30
    [D45, D45, D45, D45, D45, D45], // 31
];

/// Стратегии шести блоков для 5-битного кода кадра (старшие биты отбрасываются).
pub(crate) fn frame_exponent_strategies(code: u8) -> [ExponentStrategy; FRAME_STRATEGY_BLOCKS] {
    FRAME_EXPONENT_STRATEGIES[usize::from(code & 0x1F)]
}

#[cfg(test)]
mod tests {
    use super::{FRAME_EXPONENT_STRATEGIES, frame_exponent_strategies};
    use crate::spectral::exponents::ExponentStrategy;

    fn code(strategy: ExponentStrategy) -> usize {
        match strategy {
            ExponentStrategy::Reuse => 0,
            ExponentStrategy::D15 => 1,
            ExponentStrategy::D25 => 2,
            ExponentStrategy::D45 => 3,
        }
    }

    #[test]
    fn table_matches_spec_text_and_never_reuses_in_block_zero() {
        // Σ code·(позиция+1) по тексту таблицы (R=0, D15=1, D25=2, D45=3).
        let weighted: usize = FRAME_EXPONENT_STRATEGIES
            .iter()
            .flatten()
            .enumerate()
            .map(|(position, &strategy)| code(strategy) * (position + 1))
            .sum();
        assert_eq!(weighted, 32144);
        // Блок 0 кадра всегда передаёт экспоненты: reuse из прошлого кадра невозможен.
        assert!(
            FRAME_EXPONENT_STRATEGIES
                .iter()
                .all(|row| row[0] != ExponentStrategy::Reuse)
        );
        assert_eq!(frame_exponent_strategies(31), [ExponentStrategy::D45; 6]);
        assert_eq!(
            frame_exponent_strategies(32 + 8),
            FRAME_EXPONENT_STRATEGIES[8]
        );
    }
}
