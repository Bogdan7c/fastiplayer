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
