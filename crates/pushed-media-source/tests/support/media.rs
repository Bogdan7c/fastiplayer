//! Фикстуры и их нарезка независимым от источника разбором заголовков боксов.

use std::path::PathBuf;

/// Каталог фикстур crate-а.
pub fn own_fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Каталог фикстур протокола моста (записанные потоки и их медиа).
pub fn protocol_fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../browser-bridge-protocol/fixtures")
}

/// fMP4, разрезанный на init (`ftyp`+`moov`) и фрагменты (`moof`+`mdat`).
pub struct FragmentedMp4 {
    /// Init-сегмент.
    pub initialization: Vec<u8>,
    /// Медиасегменты по порядку.
    pub fragments: Vec<Vec<u8>>,
}

impl FragmentedMp4 {
    /// Режет файл по заголовкам боксов верхнего уровня.
    pub fn split(file: &[u8]) -> Self {
        let mut boxes = Vec::new();
        let mut position = 0;
        while position < file.len() {
            let size = u32::from_be_bytes(file[position..position + 4].try_into().expect("size"));
            let size = usize::try_from(size).expect("box size");
            boxes.push((
                &file[position + 4..position + 8],
                &file[position..position + size],
            ));
            position += size;
        }
        let mut initialization = Vec::new();
        let mut fragments = Vec::new();
        let mut current = Vec::new();
        for (box_type, bytes) in boxes {
            match box_type {
                b"ftyp" | b"moov" => initialization.extend_from_slice(bytes),
                b"moof" => current = bytes.to_vec(),
                b"mdat" => {
                    current.extend_from_slice(bytes);
                    fragments.push(std::mem::take(&mut current));
                }
                other => panic!("unexpected top-level box {other:?}"),
            }
        }
        Self {
            initialization,
            fragments,
        }
    }

    /// Init и выбранные фрагменты одним потоком байтов.
    pub fn stream_of(&self, fragment_indexes: std::ops::Range<usize>) -> Vec<u8> {
        let mut stream = self.initialization.clone();
        for fragment in &self.fragments[fragment_indexes] {
            stream.extend_from_slice(fragment);
        }
        stream
    }
}

/// `trun`: есть `data_offset`.
const TRUN_DATA_OFFSET: u32 = 0x1;
/// `trun`: есть флаги первого sample-а.
const TRUN_FIRST_SAMPLE_FLAGS: u32 = 0x4;
/// `trun`: у каждого sample-а своя длительность.
const TRUN_SAMPLE_DURATION: u32 = 0x100;
/// `trun`: у каждого sample-а свой размер.
const TRUN_SAMPLE_SIZE: u32 = 0x200;
/// `trun`: у каждого sample-а свои флаги.
const TRUN_SAMPLE_FLAGS: u32 = 0x400;
/// `trun`: у каждого sample-а сдвиг композиции.
const TRUN_SAMPLE_COMPOSITION_OFFSET: u32 = 0x800;
/// `tfhd`: есть длительность sample-а по умолчанию.
const TFHD_DEFAULT_SAMPLE_DURATION: u32 = 0x8;

/// Фрагмент (`moof`+`mdat`) с одной дорожкой, последний sample которого длиннее на
/// `extra_ticks` (единицы timescale дорожки).
///
/// Так в MP4 выглядит дыра в звуке: PTS следующего фрагмента позже конца
/// декодированного PCM, а контейнер считает время непрерывным (длинный последний
/// sample). Полка источника видит такие фрагменты вплотную и подаёт их демуксеру,
/// а плеер получает настоящий разрыв PTS (пропущенный фрагмент без удлинения полка
/// не подаёт: дыра больше 100 мс у неё — ожидание недостающего сегмента).
pub fn with_last_sample_extended(fragment: &[u8], extra_ticks: u32) -> Vec<u8> {
    let read_u32 = |bytes: &[u8], at: usize| {
        u32::from_be_bytes(bytes[at..at + 4].try_into().expect("u32 field"))
    };
    let moof_size = read_u32(fragment, 0) as usize;
    assert_eq!(&fragment[4..8], b"moof", "fragment starts with moof");
    let (moof, mdat) = fragment.split_at(moof_size);

    // Дети moof: mfhd и ровно один traf с tfhd/tfdt/trun.
    let mut new_moof_children = Vec::new();
    let mut size_delta = 0_usize;
    let mut position = 8;
    while position < moof.len() {
        let child_size = read_u32(moof, position) as usize;
        let child = &moof[position..position + child_size];
        if &child[4..8] == b"traf" {
            let (traf, delta) = traf_with_last_sample_extended(child, extra_ticks);
            new_moof_children.extend_from_slice(&traf);
            size_delta = delta;
        } else {
            new_moof_children.extend_from_slice(child);
        }
        position += child_size;
    }
    let mut new_fragment = Vec::with_capacity(fragment.len() + size_delta);
    new_fragment.extend_from_slice(
        &u32::try_from(8 + new_moof_children.len())
            .expect("moof size")
            .to_be_bytes(),
    );
    new_fragment.extend_from_slice(b"moof");
    new_fragment.extend_from_slice(&new_moof_children);
    new_fragment.extend_from_slice(mdat);
    new_fragment
}

/// `traf` с переписанным `trun`; второе значение — на сколько байт вырос `traf`.
fn traf_with_last_sample_extended(traf: &[u8], extra_ticks: u32) -> (Vec<u8>, usize) {
    let read_u32 = |bytes: &[u8], at: usize| {
        u32::from_be_bytes(bytes[at..at + 4].try_into().expect("u32 field"))
    };
    let mut default_duration = None;
    let mut children = Vec::new();
    let mut growth = 0;
    let mut position = 8;
    while position < traf.len() {
        let child_size = read_u32(traf, position) as usize;
        let child = &traf[position..position + child_size];
        match &child[4..8] {
            b"tfhd" => {
                let flags = read_u32(child, 8) & 0x00ff_ffff;
                assert_eq!(flags & 0x1, 0, "no base data offset in fixtures");
                let mut field = 16;
                if flags & 0x2 != 0 {
                    field += 4;
                }
                if flags & TFHD_DEFAULT_SAMPLE_DURATION != 0 {
                    default_duration = Some(read_u32(child, field));
                }
                children.extend_from_slice(child);
            }
            b"trun" => {
                let duration =
                    default_duration.expect("tfhd with a default duration precedes trun");
                let trun = trun_with_last_sample_extended(child, duration, extra_ticks);
                growth = trun.len() - child.len();
                children.extend_from_slice(&trun);
            }
            _ => children.extend_from_slice(child),
        }
        position += child_size;
    }
    let mut new_traf = Vec::with_capacity(8 + children.len());
    new_traf.extend_from_slice(
        &u32::try_from(8 + children.len())
            .expect("traf size")
            .to_be_bytes(),
    );
    new_traf.extend_from_slice(b"traf");
    new_traf.extend_from_slice(&children);
    (new_traf, growth)
}

/// `trun` с явной длительностью каждого sample-а (последний длиннее на `extra_ticks`).
///
/// `data_offset` отсчитывается от начала moof: moof вырастает на столько же байт, на
/// сколько `trun`, поэтому смещение данных растёт на ту же величину.
fn trun_with_last_sample_extended(trun: &[u8], default_duration: u32, extra_ticks: u32) -> Vec<u8> {
    let read_u32 = |at: usize| u32::from_be_bytes(trun[at..at + 4].try_into().expect("u32 field"));
    let version_and_flags = read_u32(8);
    let flags = version_and_flags & 0x00ff_ffff;
    let sample_count = read_u32(12) as usize;
    let mut position = 16;
    let data_offset = (flags & TRUN_DATA_OFFSET != 0).then(|| {
        position += 4;
        read_u32(position - 4)
    });
    let first_sample_flags = (flags & TRUN_FIRST_SAMPLE_FLAGS != 0).then(|| {
        position += 4;
        read_u32(position - 4)
    });
    let per_sample_fields = [
        TRUN_SAMPLE_DURATION,
        TRUN_SAMPLE_SIZE,
        TRUN_SAMPLE_FLAGS,
        TRUN_SAMPLE_COMPOSITION_OFFSET,
    ];
    let mut entries = Vec::new();
    for sample_index in 0..sample_count {
        let mut fields = [None; 4];
        for (slot, field_flag) in per_sample_fields.iter().enumerate() {
            if flags & field_flag != 0 {
                fields[slot] = Some(read_u32(position));
                position += 4;
            }
        }
        let mut duration = fields[0].unwrap_or(default_duration);
        if sample_index + 1 == sample_count {
            duration += extra_ticks;
        }
        entries.extend_from_slice(&duration.to_be_bytes());
        for value in fields[1..].iter().flatten() {
            entries.extend_from_slice(&value.to_be_bytes());
        }
    }
    let added_bytes = if flags & TRUN_SAMPLE_DURATION == 0 {
        4 * sample_count
    } else {
        0
    };
    let new_flags = (version_and_flags & 0xff00_0000) | flags | TRUN_SAMPLE_DURATION;
    let mut body = Vec::new();
    body.extend_from_slice(&new_flags.to_be_bytes());
    body.extend_from_slice(
        &u32::try_from(sample_count)
            .expect("sample count")
            .to_be_bytes(),
    );
    if let Some(data_offset) = data_offset {
        let shifted = data_offset + u32::try_from(added_bytes).expect("trun growth");
        body.extend_from_slice(&shifted.to_be_bytes());
    }
    if let Some(first_sample_flags) = first_sample_flags {
        body.extend_from_slice(&first_sample_flags.to_be_bytes());
    }
    body.extend_from_slice(&entries);
    let mut new_trun = Vec::with_capacity(8 + body.len());
    new_trun.extend_from_slice(
        &u32::try_from(8 + body.len())
            .expect("trun size")
            .to_be_bytes(),
    );
    new_trun.extend_from_slice(b"trun");
    new_trun.extend_from_slice(&body);
    new_trun
}
