//! Шрифт-приманка для проверки вложений: копия файла Noto Sans, в которой группа `cmap`
//! с буквой `A` переехала в область частного использования. Такого символа нет ни в одном
//! встроенном шрифте, поэтому реплика с ним рисуется глифом только при установленном вложении.
//!
//! Настоящее вложение фикстуры `ass-with-font.mkv` (Noto Sans Hebrew) для этого не годится:
//! тот же шрифт уже встроен в растеризатор, и картинка с вложением и без него совпадает.

use std::path::Path;

/// Начало области кодпоинтов (плоскость 15, частное использование).
const REMAPPED_BASE: u32 = 0x10_F000;

fn read_u16(bytes: &[u8], at: usize) -> usize {
    usize::from(u16::from_be_bytes([bytes[at], bytes[at + 1]]))
}

fn read_u32(bytes: &[u8], at: usize) -> usize {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize
}

/// Смещение таблицы `tag` от начала файла шрифта.
fn table_offset(bytes: &[u8], tag: &[u8; 4]) -> usize {
    let table_count = read_u16(bytes, 4);
    (0..table_count)
        .map(|index| 12 + 16 * index)
        .find(|record| &bytes[*record..*record + 4] == tag)
        .map(|record| read_u32(bytes, record + 8))
        .expect("the test font must contain the table")
}

/// Байты шрифта с перенесённой буквой `A` и символ, который теперь рисуется её глифом.
pub(super) fn font_with_private_glyph() -> (Vec<u8>, char) {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../subtitle-raster/fonts/NotoSans-Regular.ttf");
    let mut bytes = std::fs::read(path).expect("repository font file");
    let cmap = table_offset(&bytes, b"cmap");
    let mut remapped = None;
    for index in 0..read_u16(&bytes, cmap + 2) {
        let subtable = cmap + read_u32(&bytes, cmap + 4 + 8 * index + 4);
        if read_u16(&bytes, subtable) != 12 {
            continue;
        }
        let group_count = read_u32(&bytes, subtable + 12);
        let mut groups: Vec<[u32; 3]> = (0..group_count)
            .map(|group| {
                let at = subtable + 16 + 12 * group;
                [
                    read_u32(&bytes, at) as u32,
                    read_u32(&bytes, at + 4) as u32,
                    read_u32(&bytes, at + 8) as u32,
                ]
            })
            .collect();
        for group in &mut groups {
            if group[0] <= 0x41 && 0x41 <= group[1] {
                let length = group[1] - group[0];
                remapped = Some(REMAPPED_BASE + (0x41 - group[0]));
                group[0] = REMAPPED_BASE;
                group[1] = REMAPPED_BASE + length;
            }
        }
        // Поиск по cmap требует сортировки групп по началу.
        groups.sort_by_key(|group| group[0]);
        for (group, values) in groups.iter().enumerate() {
            let at = subtable + 16 + 12 * group;
            for (slot, value) in values.iter().enumerate() {
                bytes[at + 4 * slot..at + 4 * slot + 4].copy_from_slice(&value.to_be_bytes());
            }
        }
    }
    let private_char = remapped
        .and_then(char::from_u32)
        .expect("cmap format 12 with the letter A must exist");
    (bytes, private_char)
}
