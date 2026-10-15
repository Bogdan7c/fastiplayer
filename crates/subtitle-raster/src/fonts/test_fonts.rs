//! Искусственные шрифты для тестов: правка копий встроенного Noto без внешних файлов.
//!
//! Нужны, чтобы проверять fallback и сбои на настоящих байтах: шрифт с символом, которого нет
//! ни в одном встроенном, шрифт без таблицы `head` (нет метрик), коллекция `.ttc` из лиц,
//! делящих файл, и временный каталог, который `fontdb` сканирует как «системный».

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use super::embedded::{EMBEDDED_FONTS, EmbeddedFontPurpose};
use super::style::FontWeightClass;

/// Начало области кодпоинтов (плоскость 15, частное использование), которой нет во встроенных.
pub(crate) const REMAPPED_BASE: u32 = 0x10_F000;

/// Байты встроенного Noto Sans Regular.
pub(crate) fn noto_regular_bytes() -> Vec<u8> {
    EMBEDDED_FONTS
        .iter()
        .find(|font| {
            font.purpose == EmbeddedFontPurpose::PrimaryLatinGreekCyrillic
                && font.weight == FontWeightClass::Regular
        })
        .map(|font| font.data.to_vec())
        .unwrap_or_default()
}

/// Байты встроенного Noto Sans Bold.
pub(crate) fn noto_bold_bytes() -> Vec<u8> {
    EMBEDDED_FONTS
        .iter()
        .find(|font| {
            font.purpose == EmbeddedFontPurpose::PrimaryLatinGreekCyrillic
                && font.weight == FontWeightClass::Bold
        })
        .map(|font| font.data.to_vec())
        .unwrap_or_default()
}

/// Выставляет вес `usWeightClass` в таблице `OS/2` (так `fontdb` узнаёт вес системного лица).
pub(crate) fn set_os2_weight(bytes: &mut [u8], weight: u16) {
    let (_, os2) = find_table(bytes, b"OS/2");
    bytes[os2 + 4..os2 + 6].copy_from_slice(&weight.to_be_bytes());
}

fn read_u16(bytes: &[u8], at: usize) -> usize {
    usize::from(u16::from_be_bytes([bytes[at], bytes[at + 1]]))
}

fn read_u32(bytes: &[u8], at: usize) -> usize {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize
}

/// Смещение и запись таблицы `tag` в каталоге таблиц (`(позиция записи, смещение таблицы)`).
fn find_table(bytes: &[u8], tag: &[u8; 4]) -> (usize, usize) {
    let table_count = read_u16(bytes, 4);
    (0..table_count)
        .map(|index| 12 + 16 * index)
        .find(|record| &bytes[*record..*record + 4] == tag)
        .map(|record| (record, read_u32(bytes, record + 8)))
        .expect("table must exist in the test font")
}

/// Переносит группу `cmap` формата 12, содержащую `'A'`, в область [`REMAPPED_BASE`].
/// Возвращает символ, который теперь отображается в глиф бывшей `'A'`.
pub(crate) fn remap_letter_a(bytes: &mut [u8]) -> char {
    let (_, cmap) = find_table(bytes, b"cmap");
    let subtable_count = read_u16(bytes, cmap + 2);
    let mut remapped = None;
    for index in 0..subtable_count {
        let subtable = cmap + read_u32(bytes, cmap + 4 + 8 * index + 4);
        if read_u16(bytes, subtable) != 12 {
            continue;
        }
        let group_count = read_u32(bytes, subtable + 12);
        let mut groups: Vec<[u32; 3]> = (0..group_count)
            .map(|group| {
                let at = subtable + 16 + 12 * group;
                [
                    read_u32(bytes, at) as u32,
                    read_u32(bytes, at + 4) as u32,
                    read_u32(bytes, at + 8) as u32,
                ]
            })
            .collect();
        for group in &mut groups {
            if group[0] <= 0x41 && 0x41 <= group[1] {
                let length = group[1] - group[0];
                remapped = Some(REMAPPED_BASE + (0x41 - group[0]));
                // Группа целиком переезжает; начальный глиф остаётся тем же.
                group[0] = REMAPPED_BASE;
                group[1] = REMAPPED_BASE + length;
            }
        }
        // Бинарный поиск в cmap требует сортировки групп по началу.
        groups.sort_by_key(|group| group[0]);
        for (group, values) in groups.iter().enumerate() {
            let at = subtable + 16 + 12 * group;
            for (slot, value) in values.iter().enumerate() {
                bytes[at + 4 * slot..at + 4 * slot + 4].copy_from_slice(&value.to_be_bytes());
            }
        }
    }
    remapped
        .and_then(char::from_u32)
        .expect("cmap format 12 with the letter A must exist")
}

/// Переименовывает запись таблицы в каталоге (таблица «пропадает» для читателей шрифта).
pub(crate) fn rename_table(bytes: &mut [u8], from: &[u8; 4], to: &[u8; 4]) {
    let (record, _) = find_table(bytes, from);
    bytes[record..record + 4].copy_from_slice(to);
}

/// Собирает коллекцию `.ttc` из `faces` лиц, ссылающихся на одни и те же таблицы шрифта.
pub(crate) fn as_collection(font: &[u8], faces: usize) -> Vec<u8> {
    let header_len = 12 + 4 * faces;
    let mut collection = Vec::with_capacity(header_len + font.len());
    collection.extend_from_slice(b"ttcf");
    collection.extend_from_slice(&0x0001_0000_u32.to_be_bytes());
    collection.extend_from_slice(&(faces as u32).to_be_bytes());
    for _ in 0..faces {
        collection.extend_from_slice(&(header_len as u32).to_be_bytes());
    }
    collection.extend_from_slice(font);
    // Смещения таблиц в TTC считаются от начала файла: сдвигаем на длину заголовка.
    let table_count = read_u16(font, 4);
    for table in 0..table_count {
        let at = header_len + 12 + 16 * table + 8;
        let shifted = (read_u32(&collection, at) + header_len) as u32;
        collection[at..at + 4].copy_from_slice(&shifted.to_be_bytes());
    }
    collection
}

/// Временный каталог со шрифтами; удаляется при выходе из области видимости.
pub(crate) struct TempFontDir {
    path: PathBuf,
}

impl TempFontDir {
    /// Создаёт уникальный пустой каталог.
    pub(crate) fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "subtitle-raster-fonts-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        Self { path }
    }

    /// Каталог.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Записывает файл шрифта и возвращает его путь.
    pub(crate) fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let file = self.path.join(name);
        std::fs::write(&file, bytes).expect("write font file");
        file
    }
}

impl Drop for TempFontDir {
    fn drop(&mut self) {
        // Ошибка очистки временного каталога не должна ронять тест.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Портит данные контура глифа `glyph` (в таблице `glyf`): шрифт по-прежнему разбирается и
/// имеет метрики, но загрузка этого контура даёт ошибку.
pub(crate) fn corrupt_glyph_outline(bytes: &mut [u8], glyph: u16) {
    let (_, head) = find_table(bytes, b"head");
    let long_offsets = read_u16(bytes, head + 50) != 0;
    let (_, loca) = find_table(bytes, b"loca");
    let (_, glyf) = find_table(bytes, b"glyf");
    let entry = usize::from(glyph);
    let (start, end) = if long_offsets {
        (
            read_u32(bytes, loca + 4 * entry),
            read_u32(bytes, loca + 4 * entry + 4),
        )
    } else {
        (
            2 * read_u16(bytes, loca + 2 * entry),
            2 * read_u16(bytes, loca + 2 * entry + 2),
        )
    };
    assert!(end > start + 12, "the glyph must have outline data");
    // Заголовок глифа (число контуров и рамка, 10 байт) остаётся, поэтому глиф «существует»;
    // всё после него (конечные точки контуров, флаги, координаты) забито мусором.
    for byte in &mut bytes[glyf + start + 10..glyf + end] {
        *byte = 0xFF;
    }
}

/// Заменяет строку `from` на `to` (той же длины) в таблице `name`: и в UTF-16BE (Windows),
/// и в однобайтовом виде (Mac). Так `fontdb` видит у копии шрифта другое имя семейства.
pub(crate) fn rename_family(bytes: &mut [u8], from: &str, to: &str) {
    assert_eq!(
        from.chars().count(),
        to.chars().count(),
        "equal length only"
    );
    let utf16 = |text: &str| -> Vec<u8> {
        text.encode_utf16()
            .flat_map(|unit| unit.to_be_bytes())
            .collect()
    };
    for (old, new) in [
        (utf16(from), utf16(to)),
        (from.as_bytes().to_vec(), to.as_bytes().to_vec()),
    ] {
        let mut at = 0;
        while at + old.len() <= bytes.len() {
            if bytes[at..at + old.len()] == old[..] {
                bytes[at..at + old.len()].copy_from_slice(&new);
                at += old.len();
            } else {
                at += 1;
            }
        }
    }
}

/// Добавляет в одиночный шрифт (не `.ttc`) таблицу `meta` с записью `dlng` = `languages`
/// (список тегов через запятую, например `"Jpan"`). Каталог таблиц остаётся отсортированным.
pub(crate) fn add_meta_dlng(font: &[u8], languages: &str) -> Vec<u8> {
    let mut meta = Vec::new();
    meta.extend_from_slice(&1_u32.to_be_bytes()); // version
    meta.extend_from_slice(&0_u32.to_be_bytes()); // flags
    meta.extend_from_slice(&0_u32.to_be_bytes()); // reserved
    meta.extend_from_slice(&1_u32.to_be_bytes()); // dataMapsCount
    meta.extend_from_slice(b"dlng");
    meta.extend_from_slice(&28_u32.to_be_bytes()); // dataOffset: заголовок 16 + запись 12
    meta.extend_from_slice(&(languages.len() as u32).to_be_bytes());
    meta.extend_from_slice(languages.as_bytes());
    add_table(font, b"meta", &meta)
}

/// Собирает новый файл шрифта с дополнительной таблицей `tag` (контрольные суммы не нужны:
/// читатели шрифтов их не проверяют).
fn add_table(font: &[u8], tag: &[u8; 4], content: &[u8]) -> Vec<u8> {
    let table_count = read_u16(font, 4);
    let old_records_end = 12 + 16 * table_count;
    let mut records: Vec<[u8; 16]> = (0..table_count)
        .map(|index| {
            let at = 12 + 16 * index;
            let mut record = [0_u8; 16];
            record.copy_from_slice(&font[at..at + 16]);
            // Данные уехали на одну запись (16 байт) дальше от начала файла.
            let shifted = (read_u32(font, at + 8) + 16) as u32;
            record[8..12].copy_from_slice(&shifted.to_be_bytes());
            record
        })
        .collect();
    let mut data: Vec<u8> = font[old_records_end..].to_vec();
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
    let new_offset = (old_records_end + 16 + data.len()) as u32;
    let mut new_record = [0_u8; 16];
    new_record[..4].copy_from_slice(tag);
    new_record[8..12].copy_from_slice(&new_offset.to_be_bytes());
    new_record[12..16].copy_from_slice(&(content.len() as u32).to_be_bytes());
    records.push(new_record);
    records.sort_by_key(|record| [record[0], record[1], record[2], record[3]]);

    let mut result = Vec::with_capacity(old_records_end + 16 + data.len() + content.len());
    result.extend_from_slice(&font[..12]);
    result[4..6].copy_from_slice(&((table_count + 1) as u16).to_be_bytes());
    for record in &records {
        result.extend_from_slice(record);
    }
    result.extend_from_slice(&data);
    result.extend_from_slice(content);
    result
}
