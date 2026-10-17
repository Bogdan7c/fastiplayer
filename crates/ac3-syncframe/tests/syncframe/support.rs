//! Ручная сборка заголовков AC-3/E-AC-3 бит за битом по A/52 — независимо от
//! кода разбора, чтобы тест не проверял парсер самим собой.

/// MSB-first запись полей, как они лежат в битовом потоке.
#[derive(Default)]
pub struct BitWriter {
    bytes: Vec<u8>,
    bit_count: usize,
}

impl BitWriter {
    pub fn put(&mut self, width: usize, value: u32) -> &mut Self {
        for shift in (0..width).rev() {
            if self.bit_count.is_multiple_of(8) {
                self.bytes.push(0);
            }
            let bit = ((value >> shift) & 1) as u8;
            let last = self.bytes.last_mut().expect("byte pushed above");
            *last |= bit << (7 - self.bit_count % 8);
            self.bit_count += 1;
        }
        self
    }

    /// Дополняет нулями до `total_bytes` (полезная нагрузка кадра тестам не важна).
    pub fn finish_padded(&self, total_bytes: usize) -> Vec<u8> {
        let mut bytes = self.bytes.clone();
        assert!(bytes.len() <= total_bytes, "header longer than frame");
        bytes.resize(total_bytes, 0);
        bytes
    }

    pub fn header_bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }
}

/// Поля E-AC-3 syncframe-а, которые важны demuxer-у.
#[derive(Clone, Copy)]
pub struct Eac3Frame {
    pub stream_type: u32,
    pub substream_id: u32,
    pub frame_bytes: usize,
    pub fscod: u32,
    /// numblkscod при fscod < 3, fscod2 при fscod == 3.
    pub numblkscod_or_fscod2: u32,
    pub acmod: u32,
    pub lfeon: bool,
    pub bsid: u32,
    pub chanmap: Option<u16>,
}

impl Eac3Frame {
    /// Независимый substream 0, 48 кГц, 6 блоков, 5.1.
    pub const fn independent_5p1() -> Self {
        Self {
            stream_type: 0,
            substream_id: 0,
            frame_bytes: 64,
            fscod: 0,
            numblkscod_or_fscod2: 3,
            acmod: 7,
            lfeon: true,
            bsid: 16,
            chanmap: None,
        }
    }

    /// Dependent substream 0 с парой Lrs/Rrs (позиция 6) — расширение до 7.1.
    pub const fn dependent_back_pair() -> Self {
        Self {
            stream_type: 1,
            substream_id: 0,
            frame_bytes: 48,
            fscod: 0,
            numblkscod_or_fscod2: 3,
            acmod: 2,
            lfeon: false,
            bsid: 16,
            chanmap: Some(1 << (15 - 6)),
        }
    }

    pub fn encode(self) -> Vec<u8> {
        let mut writer = BitWriter::default();
        writer
            .put(16, 0x0b77)
            .put(2, self.stream_type)
            .put(3, self.substream_id)
            .put(11, (self.frame_bytes / 2 - 1) as u32)
            .put(2, self.fscod)
            .put(2, self.numblkscod_or_fscod2)
            .put(3, self.acmod)
            .put(1, u32::from(self.lfeon))
            .put(5, self.bsid)
            .put(5, 27) // dialnorm
            .put(1, 1) // compre
            .put(8, 0x5a); // compr
        if self.acmod == 0 {
            writer.put(5, 27).put(1, 0); // dialnorm2, compr2e
        }
        if self.stream_type == 1 {
            match self.chanmap {
                Some(chanmap) => writer.put(1, 1).put(16, u32::from(chanmap)),
                None => writer.put(1, 0),
            };
        }
        writer.finish_padded(self.frame_bytes)
    }
}

/// Только заголовок AC-3 (syncinfo + начало BSI); payload парсеру не нужен.
pub fn ac3_header(fscod: u32, frmsizecod: u32, bsid: u32, acmod: u32, lfeon: bool) -> Vec<u8> {
    let mut writer = BitWriter::default();
    writer
        .put(16, 0x0b77)
        .put(16, 0xabcd) // crc1
        .put(2, fscod)
        .put(6, frmsizecod)
        .put(5, bsid)
        .put(3, 0) // bsmod
        .put(3, acmod);
    if acmod & 1 != 0 && acmod != 1 {
        writer.put(2, 0); // cmixlev
    }
    if acmod & 4 != 0 {
        writer.put(2, 0); // surmixlev
    }
    if acmod == 2 {
        writer.put(2, 0); // dsurmod
    }
    writer.put(1, u32::from(lfeon));
    // Хвост BSI, которого парсер не читает, — нули до конца байта и ещё 4 байта.
    writer.put(32, 0);
    writer.header_bytes()
}

/// Полный AC-3 кадр 48 кГц нужной длины (frmsizecod выбирается по битрейту).
pub fn ac3_frame_48k(frmsizecod: u32, acmod: u32, lfeon: bool) -> Vec<u8> {
    let mut frame = ac3_header(0, frmsizecod, 8, acmod, lfeon);
    let words = [
        64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 640, 768, 896, 1024, 1152,
        1280,
    ];
    frame.resize(words[(frmsizecod / 2) as usize] * 2, 0);
    frame
}
