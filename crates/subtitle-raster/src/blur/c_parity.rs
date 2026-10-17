//! Сравнение с живой libass 0.17.5 (C-движок, `align_order = 4`): побитовое совпадение и
//! скорость. Тест `#[ignore]` — репозиторий собирается и проверяется без C.
//!
//! # Как запускать
//!
//! 1. Собрать libass без ассемблера (`libass_noasm.so`) и харнесс `blurh` (исходник живёт
//!    вне репозитория, в рабочей папке сессии; протокол: `blurh run <in> <out> <r2x> <r2y>
//!    <be> <iters>` читает/пишет файл `left top w h (i32) stride (i64) buffer`, печатает
//!    `min_ns median_ns` времени одного `ass_synth_blur`).
//! 2. `LIBASS_BLUR_HARNESS=/путь/к/blurh cargo test -p subtitle-raster --release --locked
//!    --lib blur::c_parity -- --ignored --nocapture`.
//!
//! Опционально `LIBASS_BLUR_WORKDIR` — папка для временных файлов.

use super::{
    BitmapSideLimit, BlurScratch, BlurStrength, BoxBlurPasses, GaussianVariance, quantize_blur,
    restore_blur, synth_blur,
};
use crate::bitmap::CoverageBitmap;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

/// Детерминированный генератор (xorshift64*), чтобы входы были одинаковы при каждом запуске.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Вид входной картинки.
#[derive(Clone, Copy, Debug)]
enum Kind {
    /// Равномерный шум 0..=255 (худший случай для округлений).
    Noise,
    /// Похоже на глиф: мягкие эллипсы и штрихи на нулевом фоне.
    GlyphLike,
}

fn make_bitmap(kind: Kind, width: i32, height: i32, seed: u64) -> CoverageBitmap {
    let mut bitmap = CoverageBitmap::zeroed(width, height).expect("size");
    bitmap.left = 3;
    bitmap.top = -7;
    let mut rng = Rng(seed | 1);
    let (w, h, stride) = (width as usize, height as usize, bitmap.stride);
    match kind {
        Kind::Noise => {
            for y in 0..h {
                for x in 0..w {
                    bitmap.buffer[y * stride + x] = (rng.next() >> 24) as u8;
                }
            }
        }
        Kind::GlyphLike => {
            // Несколько колец и прямоугольных штрихов с антиалиасингом по краю.
            let shapes = 4 + (w * h / 4000).min(12);
            for _ in 0..shapes {
                let cx = (rng.next() % w as u64) as f64;
                let cy = (rng.next() % h as u64) as f64;
                let rx = 2.0 + (rng.next() % (w as u64 / 3 + 2)) as f64;
                let ry = 2.0 + (rng.next() % (h as u64 / 3 + 2)) as f64;
                let ring = rng.next().is_multiple_of(2);
                for y in 0..h {
                    for x in 0..w {
                        let dx = (x as f64 - cx) / rx;
                        let dy = (y as f64 - cy) / ry;
                        let dist = (dx * dx + dy * dy).sqrt();
                        let coverage = if ring {
                            1.0 - ((dist - 0.8).abs() * rx.min(ry) * 1.2).min(1.0)
                        } else {
                            (1.0 - dist) * rx.min(ry)
                        };
                        let value = (coverage.clamp(0.0, 1.0) * 255.0) as u8;
                        let cell = &mut bitmap.buffer[y * stride + x];
                        *cell = (*cell).max(value);
                    }
                }
            }
        }
    }
    bitmap
}

fn write_bitmap(path: &PathBuf, bitmap: &CoverageBitmap) {
    let mut bytes = Vec::new();
    for value in [bitmap.left, bitmap.top, bitmap.width, bitmap.height] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&(bitmap.stride as i64).to_le_bytes());
    bytes.extend_from_slice(&bitmap.buffer);
    std::fs::write(path, bytes).expect("write input");
}

fn read_bitmap(path: &PathBuf) -> CoverageBitmap {
    let bytes = std::fs::read(path).expect("read output");
    let int = |at: usize| i32::from_le_bytes(bytes[at..at + 4].try_into().expect("i32"));
    let stride = i64::from_le_bytes(bytes[16..24].try_into().expect("i64")) as usize;
    CoverageBitmap {
        left: int(0),
        top: int(4),
        width: int(8),
        height: int(12),
        stride,
        buffer: bytes[24..].to_vec(),
    }
}

/// Медиана и минимум времени одного вызова размытия в нс.
fn time_rust(input: &CoverageBitmap, strength: BlurStrength, iterations: usize) -> (f64, f64) {
    let mut scratch = BlurScratch::new();
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let mut bitmap = input.clone();
        let start = Instant::now();
        synth_blur(
            &mut scratch,
            &mut bitmap,
            strength,
            BitmapSideLimit::UNLIMITED,
        )
        .expect("blur");
        samples.push(start.elapsed().as_nanos() as f64);
        std::hint::black_box(&bitmap);
    }
    samples.sort_by(f64::total_cmp);
    (samples[0], samples[iterations / 2])
}

fn r2_for_blur(blur: f64) -> f64 {
    restore_blur(quantize_blur(blur * super::blur_radius_scale()).level)
}

#[test]
#[ignore = "нужна живая libass: см. документацию модуля"]
fn gaussian_and_box_blur_match_libass_bit_for_bit() {
    let harness = std::env::var("LIBASS_BLUR_HARNESS").expect("LIBASS_BLUR_HARNESS");
    let workdir = std::env::var("LIBASS_BLUR_WORKDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("subtitle-blur-parity"));
    std::fs::create_dir_all(&workdir).expect("workdir");

    let sizes = [(17, 9), (64, 64), (300, 120), (1700, 400)];
    let gaussians: Vec<(String, f64, f64)> = [0.1, 0.5, 2.0, 8.0, 30.0, 100.0]
        .iter()
        .map(|&blur| (format!("blur{blur}"), r2_for_blur(blur), r2_for_blur(blur)))
        .chain([
            ("aniso2x30".to_string(), r2_for_blur(2.0), r2_for_blur(30.0)),
            ("x8y0".to_string(), r2_for_blur(8.0), 0.0),
            ("x0y8".to_string(), 0.0, r2_for_blur(8.0)),
            ("tiny".to_string(), 0.0015, 0.0005),
        ])
        .collect();
    let box_passes = [1, 2, 3, 10, 127];

    let mut strengths: Vec<(String, f64, f64, i32)> = Vec::new();
    for (name, r2x, r2y) in &gaussians {
        strengths.push((name.clone(), *r2x, *r2y, 0));
    }
    for be in box_passes {
        strengths.push((format!("be{be}"), 0.0, 0.0, be));
    }
    strengths.push((
        "blur2+be2".to_string(),
        r2_for_blur(2.0),
        r2_for_blur(2.0),
        2,
    ));
    strengths.push((
        "blur30+be5".to_string(),
        r2_for_blur(30.0),
        r2_for_blur(30.0),
        5,
    ));

    println!(
        "{:<8} {:<7} {:<11} | {:>12} {:>12} | {:>12} {:>12} | {:>6} | diff",
        "size", "kind", "case", "C min ns", "C med ns", "Rust min ns", "Rust med ns", "R/C med"
    );
    let mut worst_diff = 0u32;
    let mut cases = 0;
    let mut shared_scratch = BlurScratch::new();
    for (width, height) in sizes {
        for kind in [Kind::Noise, Kind::GlyphLike] {
            let input = make_bitmap(kind, width, height, 0x9E37_79B9 ^ (width as u64) << 8);
            let in_path = workdir.join("in.bin");
            write_bitmap(&in_path, &input);
            for (name, r2x, r2y, be) in &strengths {
                // be-серии на огромной картинке дороги; они всё равно проверяются.
                let iterations = (40_000_000 / (width * height) as usize).clamp(15, 20_000) | 1;
                let out_path = workdir.join("out.bin");
                let output = Command::new(&harness)
                    .arg("run")
                    .arg(&in_path)
                    .arg(&out_path)
                    .arg(format!("{r2x:.17e}"))
                    .arg(format!("{r2y:.17e}"))
                    .arg(be.to_string())
                    .arg(iterations.to_string())
                    .output()
                    .expect("harness");
                assert!(output.status.success(), "harness failed: {output:?}");
                let times: Vec<f64> = String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .map(|v| v.parse().expect("time"))
                    .collect();
                let expected = read_bitmap(&out_path);

                let strength = BlurStrength {
                    gaussian: GaussianVariance { x: *r2x, y: *r2y },
                    box_passes: BoxBlurPasses::clamped(*be),
                };
                // Строка r2, прошедшая через `{:e}`, должна читаться C-кодом в то же f64.
                // Одна рабочая память на весь прогон: заодно проверяется переиспользование буферов.
                let mut actual = input.clone();
                synth_blur(
                    &mut shared_scratch,
                    &mut actual,
                    strength,
                    BitmapSideLimit::UNLIMITED,
                )
                .expect("blur");
                assert_eq!(
                    (
                        actual.left,
                        actual.top,
                        actual.width,
                        actual.height,
                        actual.stride
                    ),
                    (
                        expected.left,
                        expected.top,
                        expected.width,
                        expected.height,
                        expected.stride
                    ),
                    "geometry {width}x{height} {kind:?} {name}"
                );
                let diff = actual
                    .buffer
                    .iter()
                    .zip(&expected.buffer)
                    .map(|(a, b)| u32::from(a.abs_diff(*b)))
                    .max()
                    .unwrap_or(0);
                assert_eq!(actual.buffer.len(), expected.buffer.len());
                worst_diff = worst_diff.max(diff);
                let (rust_min, rust_med) = time_rust(&input, strength, iterations);
                println!(
                    "{:<8} {:<7} {:<11} | {:>12.0} {:>12.0} | {:>12.0} {:>12.0} | {:>6.2} | {}",
                    format!("{width}x{height}"),
                    format!("{kind:?}").chars().take(7).collect::<String>(),
                    name,
                    times[0],
                    times[1],
                    rust_min,
                    rust_med,
                    rust_med / times[1],
                    diff
                );
                cases += 1;
            }
        }
    }
    println!("cases: {cases}, max diff: {worst_diff}");
    assert_eq!(worst_diff, 0);
}

/// Генератор золотых файлов `tests/fixtures/blur/*.{in,out}.bin`: входы строятся здесь,
/// выходы снимает живая libass через харнесс. Нужны `LIBASS_BLUR_HARNESS` и
/// `LIBASS_BLUR_FIXTURE_DIR` (папка `crates/subtitle-raster/tests/fixtures/blur`).
#[test]
#[ignore = "перегенерирует золотые файлы; нужна живая libass"]
fn regenerate_golden_fixtures() {
    use super::tests::golden::GOLDEN_CASES;

    let harness = std::env::var("LIBASS_BLUR_HARNESS").expect("LIBASS_BLUR_HARNESS");
    let dir =
        PathBuf::from(std::env::var("LIBASS_BLUR_FIXTURE_DIR").expect("LIBASS_BLUR_FIXTURE_DIR"));
    std::fs::create_dir_all(&dir).expect("fixture dir");
    let inputs = [
        make_bitmap(Kind::GlyphLike, 17, 9, 101),
        make_bitmap(Kind::GlyphLike, 40, 24, 202),
        make_bitmap(Kind::Noise, 33, 20, 303),
        make_bitmap(Kind::Noise, 12, 6, 404),
    ];
    for (case, input) in GOLDEN_CASES.iter().zip(&inputs) {
        let in_path = dir.join(format!("{}.in.bin", case.name));
        let out_path = dir.join(format!("{}.out.bin", case.name));
        write_bitmap(&in_path, input);
        let status = Command::new(&harness)
            .arg("run")
            .arg(&in_path)
            .arg(&out_path)
            .arg(format!("{:.17e}", case.r2_x))
            .arg(format!("{:.17e}", case.r2_y))
            .arg(case.box_passes.to_string())
            .arg("1")
            .status()
            .expect("harness");
        assert!(status.success());
    }
}
