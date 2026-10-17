//! Сборщик синтаксически корректных «тихих» кадров E-AC-3 побитно
//! (Annex E Таблицы E1.1–E1.6): все SNR offset-ы нулевые, поэтому bap = 0 и
//! мантисс нет; экспоненты D45 только в блоке 0. Нужен для того, что кодер
//! ffmpeg не выдаёт: зависимые substream-ы и `chanmap`, другие программы,
//! пониженные частоты, 1/2 блока, синтаксис AHT/SPX, запрещённые значения.

use crate::bitstream::crc::crc16_register;

/// Писатель бит MSB-first.
#[derive(Default)]
pub(crate) struct BitWriter {
    pub(crate) bytes: Vec<u8>,
    bits: usize,
}

impl BitWriter {
    pub(crate) fn put(&mut self, value: u32, width: u32) {
        for bit in (0..width).rev() {
            if self.bits.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if (value >> bit) & 1 == 1 {
                let last = self.bytes.len() - 1;
                self.bytes[last] |= 0x80 >> (self.bits % 8);
            }
            self.bits += 1;
        }
    }

    pub(crate) fn flag(&mut self, value: bool) {
        self.put(u32::from(value), 1);
    }

    pub(crate) fn bit_len(&self) -> usize {
        self.bits
    }
}

/// Параметры собираемого кадра.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FrameSpec {
    /// `strmtyp` (0–3; 3 — зарезервирован, для тестов ошибок).
    pub(crate) stream_type: u32,
    pub(crate) substream_id: u32,
    /// `fscod` (3 — пониженная частота по `fscod2`).
    pub(crate) fscod: u32,
    /// `fscod2` при `fscod = 3`, иначе `numblkscod`.
    pub(crate) second_code: u32,
    pub(crate) acmod: u32,
    pub(crate) lfe_on: bool,
    pub(crate) channel_map: Option<u16>,
    /// `ahte` + AHT для всех fbw каналов и LFE (только при 6 блоках).
    pub(crate) aht: bool,
    /// SPX во всех блоках (`spxbegf = 2`, `spxendf = 2`, `spxstrtf = 0`).
    pub(crate) spx: bool,
    /// `snroffststr` (3 — зарезервирован).
    pub(crate) snr_strategy: u32,
    /// Длина кадра в 16-битных словах.
    pub(crate) words: usize,
    /// `compre = 1` и это слово `compr` (иначе `compre = 0`).
    pub(crate) compression: Option<u8>,
    /// `dynrnge = 1` и это слово `dynrng` в блоке 0 (дальше — повтор, §7.7.1).
    pub(crate) dynamic_range: Option<u8>,
    /// TPNP одного fbw канала: (канал, `transprocloc`, `transproclen`).
    pub(crate) transient_pre_noise: Option<(usize, u16, u8)>,
}

impl FrameSpec {
    /// Независимый substream 0: 48 кГц, 6 блоков, 2/0.
    pub(crate) const fn independent(acmod: u32, lfe_on: bool) -> Self {
        Self {
            stream_type: 0,
            substream_id: 0,
            fscod: 0,
            second_code: 3,
            acmod,
            lfe_on,
            channel_map: None,
            aht: false,
            spx: false,
            snr_strategy: 0,
            words: 256,
            compression: None,
            dynamic_range: None,
            transient_pre_noise: None,
        }
    }

    /// Зависимый substream с `chanmap`.
    pub(crate) const fn dependent(acmod: u32, lfe_on: bool, channel_map: Option<u16>) -> Self {
        Self {
            stream_type: 1,
            channel_map,
            ..Self::independent(acmod, lfe_on)
        }
    }

    fn blocks(&self) -> usize {
        if self.fscod == 3 {
            6
        } else {
            [1, 2, 3, 6][self.second_code as usize]
        }
    }

    fn channels(&self) -> usize {
        [2, 1, 2, 3, 3, 4, 4, 5][self.acmod as usize]
    }
}

/// `spx_begin_subbnd` при `spxbegf = 2` и бин начала SPX.
const SPX_BEGIN_CODE: u32 = 2;
const SPX_END_CODE: u32 = 2;
const SPX_BEGIN_BIN: usize = 73;
/// `chbwcod = 0` → `endmant = 73`.
const BANDWIDTH_CODE: u32 = 0;
const END_MANTISSA: usize = 73;
/// Групповой код «все разности 0» (25·2 + 5·2 + 2).
const FLAT_EXPONENT_GROUP: u32 = 62;

/// Собирает кадр по `spec` с корректной CRC.
pub(crate) fn build_frame(spec: &FrameSpec) -> Vec<u8> {
    let mut writer = BitWriter::default();
    writer.put(0x0B77, 16);
    writer.put(spec.stream_type, 2);
    writer.put(spec.substream_id, 3);
    writer.put((spec.words - 1) as u32, 11);
    writer.put(spec.fscod, 2);
    writer.put(spec.second_code, 2);
    write_bsi(&mut writer, spec);
    write_audio_frame(&mut writer, spec);
    for block in 0..spec.blocks() {
        write_block(&mut writer, spec, block);
    }
    finish(writer, spec.words)
}

fn write_bsi(writer: &mut BitWriter, spec: &FrameSpec) {
    writer.put(spec.acmod, 3);
    writer.flag(spec.lfe_on);
    writer.put(16, 5); // bsid
    writer.put(31, 5); // dialnorm
    writer.flag(spec.compression.is_some()); // compre
    if let Some(word) = spec.compression {
        writer.put(u32::from(word), 8);
    }
    if spec.acmod == 0 {
        writer.put(31, 5);
        writer.flag(false);
    }
    if spec.stream_type == 1 {
        writer.flag(spec.channel_map.is_some());
        if let Some(map) = spec.channel_map {
            writer.put(u32::from(map), 16);
        }
    }
    writer.flag(false); // mixmdate
    writer.flag(false); // infomdate
    if spec.stream_type == 0 && spec.blocks() != 6 {
        writer.flag(false); // convsync
    }
    if spec.stream_type == 2 {
        if spec.blocks() != 6 {
            writer.flag(true); // blkid
        }
        writer.put(20, 6); // frmsizecod
    }
    writer.flag(false); // addbsie
}

fn write_audio_frame(writer: &mut BitWriter, spec: &FrameSpec) {
    let blocks = spec.blocks();
    let six = blocks == 6;
    if six {
        writer.flag(true); // expstre
        writer.flag(spec.aht); // ahte
    }
    writer.put(spec.snr_strategy, 2);
    writer.flag(spec.transient_pre_noise.is_some()); // transproce
    // blkswe, dithflage, bamode, frmfgaincode, dbaflde, skipflde, spxattene.
    writer.put(0, 7);
    if spec.acmod > 1 {
        writer.flag(false); // cplinu[0]
        for _ in 1..blocks {
            writer.flag(false); // cplstre[blk]
        }
    }
    for block in 0..blocks {
        for _ in 0..spec.channels() {
            writer.put(if block == 0 { 3 } else { 0 }, 2); // chexpstr: D45, затем reuse
        }
    }
    if spec.lfe_on {
        for block in 0..blocks {
            writer.flag(block == 0);
        }
    }
    if spec.stream_type == 0 {
        if !six {
            writer.flag(false); // convexpstre
        } else {
            writer.put(0, 5 * spec.channels() as u32); // convexpstr
        }
    }
    if spec.aht {
        // ncplblks ≠ 6 → cplahtinu не передаётся; nchregs = nlferegs = 1.
        for _ in 0..spec.channels() {
            writer.flag(true);
        }
        if spec.lfe_on {
            writer.flag(true);
        }
    }
    if spec.snr_strategy == 0 {
        writer.put(0, 6 + 4); // frmcsnroffst, frmfsnroffst
    }
    if let Some((channel, location, length)) = spec.transient_pre_noise {
        for index in 0..spec.channels() {
            writer.flag(index == channel); // chintransproc
            if index == channel {
                writer.put(u32::from(location), 10);
                writer.put(u32::from(length), 8);
            }
        }
    }
    if blocks != 1 {
        writer.flag(false); // blkstrtinfoe
    }
}

fn write_block(writer: &mut BitWriter, spec: &FrameSpec, block: usize) {
    let channels = spec.channels();
    match spec.dynamic_range {
        Some(word) if block == 0 => {
            writer.flag(true); // dynrnge
            writer.put(u32::from(word), 8);
        }
        _ => writer.flag(false),
    }
    if spec.acmod == 0 {
        writer.flag(false);
    }
    write_spx(writer, spec, block);
    if spec.acmod == 2 {
        if block == 0 {
            // nrematbd: без coupling, со SPX при spxbegf ≥ 2 → 4, без SPX → 4.
            writer.put(0, 4);
        } else {
            writer.flag(false); // rematstr
        }
    }
    if block == 0 {
        for _ in 0..channels {
            if !spec.spx {
                writer.put(BANDWIDTH_CODE, 6);
            }
        }
        let end = if spec.spx {
            SPX_BEGIN_BIN
        } else {
            END_MANTISSA
        };
        for _ in 0..channels {
            writer.put(10, 4); // exps[ch][0]
            for _ in 0..(end - 1 + 9) / 12 {
                writer.put(FLAT_EXPONENT_GROUP, 7);
            }
            writer.put(0, 2); // gainrng
        }
        if spec.lfe_on {
            writer.put(10, 4);
            writer.put(FLAT_EXPONENT_GROUP, 7);
            writer.put(FLAT_EXPONENT_GROUP, 7);
        }
    }
    if spec.snr_strategy != 0 && spec.snr_strategy != 3 {
        if block == 0 {
            writer.put(0, 6); // csnroffst
            let fine_words = if spec.snr_strategy == 1 {
                1
            } else {
                channels + usize::from(spec.lfe_on)
            };
            writer.put(0, 4 * fine_words as u32);
        } else {
            writer.flag(false); // snroffste
        }
    }
    if spec.stream_type == 0 {
        writer.flag(false); // convsnroffste
    }
    if spec.aht && block == 0 {
        // chgaqmod = 0; все hebap = 0 → ни одного бита мантисс.
        for _ in 0..channels + usize::from(spec.lfe_on) {
            writer.put(0, 2);
        }
    }
}

/// SPX: в блоке 0 стратегия (spxbegf 2 → sub-band 4, spxendf 2 → 7) и
/// координаты; дальше `spxstre = 0` и `spxcoe = 0`.
fn write_spx(writer: &mut BitWriter, spec: &FrameSpec, block: usize) {
    if block != 0 {
        writer.flag(false); // spxstre
        if spec.spx {
            for _ in 0..spec.channels() {
                writer.flag(false); // spxcoe
            }
        }
        return;
    }
    writer.flag(spec.spx); // spxinu (spxstre = 1 неявно)
    if !spec.spx {
        return;
    }
    if spec.acmod != 1 {
        for _ in 0..spec.channels() {
            writer.flag(true); // chinspx
        }
    }
    writer.put(0, 2); // spxstrtf
    writer.put(SPX_BEGIN_CODE, 3);
    writer.put(SPX_END_CODE, 3);
    writer.flag(false); // spxbndstrce: полосы по умолчанию (sub-band-ы 4–6 — три полосы)
    for _ in 0..spec.channels() {
        writer.put(16, 5); // spxblnd
        writer.put(0, 2); // mstrspxco
        for _ in 0..3 {
            writer.put(1, 4); // spxcoexp
            writer.put(2, 2); // spxcomant
        }
    }
}

/// Добивает кадр нулями до `words` слов, пишет хвост и CRC.
fn finish(mut writer: BitWriter, words: usize) -> Vec<u8> {
    let total_bits = words * 16;
    let trailer_bits = 1 + 1 + 16; // auxdatae, encinfo, crc2
    assert!(
        writer.bit_len() + trailer_bits <= total_bits,
        "frame too small: {}",
        writer.bit_len()
    );
    while writer.bit_len() < total_bits - 16 {
        writer.flag(false);
    }
    let mut frame = writer.bytes;
    let register = crc16_register(&frame[2..]);
    frame.extend_from_slice(&register.to_be_bytes());
    frame
}

/// Пересчитывает CRC кадра после ручной правки его байтов.
pub(crate) fn refresh_crc(frame: &mut [u8]) {
    let length = frame.len();
    let register = crc16_register(&frame[2..length - 2]);
    frame[length - 2..].copy_from_slice(&register.to_be_bytes());
}
