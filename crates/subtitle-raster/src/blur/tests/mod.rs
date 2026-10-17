//! Тесты размытия: параметры, коэффициенты, ядра, box-размытие и золотые файлы libass.
//!
//! Константы «из libass» сняты с живой libass 0.17.5 (C-движок, без ассемблера) харнессом,
//! описанным в `c_parity.rs`; золотые файлы в `tests/fixtures/blur/` генерирует
//! `c_parity::regenerate_golden_fixtures` (`#[ignore]`).

use super::*;

mod box_blur;
mod gaussian;
pub(super) mod golden;
mod kernels;
mod params;

// ---------------------------------------------------------------------------------------
// Вспомогательные построения
// ---------------------------------------------------------------------------------------

/// Битмап `width x height` из значений `rows`; всё правее `width` нулевое.
fn bitmap_from_rows(rows: &[&[u8]]) -> CoverageBitmap {
    let height = rows.len();
    let width = rows.first().map_or(0, |row| row.len());
    let mut bitmap = CoverageBitmap::zeroed(width as i32, height as i32).expect("size");
    for (y, row) in rows.iter().enumerate() {
        bitmap.buffer[y * bitmap.stride..][..width].copy_from_slice(row);
    }
    bitmap
}

/// Видимая часть битмапа как список строк.
fn visible_rows(bitmap: &CoverageBitmap) -> Vec<Vec<u8>> {
    (0..bitmap.height as usize)
        .map(|y| bitmap.buffer[y * bitmap.stride..][..bitmap.width as usize].to_vec())
        .collect()
}

/// Все байты правее `width` (до шага строки) нулевые.
fn padding_is_zero(bitmap: &CoverageBitmap) -> bool {
    (0..bitmap.height as usize).all(|y| {
        bitmap.buffer[y * bitmap.stride + bitmap.width as usize..(y + 1) * bitmap.stride]
            .iter()
            .all(|&byte| byte == 0)
    })
}

/// Битмап `width x height` с одним пикселем покрытия 255 в `(x, y)`.
fn impulse(width: i32, height: i32, x: usize, y: usize) -> CoverageBitmap {
    let mut bitmap = CoverageBitmap::zeroed(width, height).expect("size");
    bitmap.buffer[y * bitmap.stride + x] = 255;
    bitmap
}

/// Детерминированный шум (xorshift64*).
fn noise_bytes(count: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..count)
        .map(|_| {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8
        })
        .collect()
}

fn noise_bitmap(width: i32, height: i32, seed: u64) -> CoverageBitmap {
    let mut bitmap = CoverageBitmap::zeroed(width, height).expect("size");
    let noise = noise_bytes((width * height) as usize, seed);
    for y in 0..height as usize {
        bitmap.buffer[y * bitmap.stride..][..width as usize]
            .copy_from_slice(&noise[y * width as usize..][..width as usize]);
    }
    bitmap
}

fn variance(x: f64, y: f64) -> GaussianVariance {
    GaussianVariance { x, y }
}

fn gaussian(bitmap: &mut CoverageBitmap, x: f64, y: f64) -> Result<(), BlurError> {
    gaussian_blur(
        &mut BlurScratch::new(),
        bitmap,
        variance(x, y),
        BitmapSideLimit::UNLIMITED,
    )
}
