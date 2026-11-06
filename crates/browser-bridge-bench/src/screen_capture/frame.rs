//! Кадр экрана и разбор PNG через `ffmpeg` (он и так обязателен для стенда).

use std::path::Path;
use std::process::Command;

use super::ScreenError;

/// Кадр в формате RGB, по три байта на пиксель, строки сверху вниз без выравнивания.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

/// Изображение с прозрачностью (значок): RGBA, по четыре байта на пиксель.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    /// Ширина, пикселей.
    pub width: u32,
    /// Высота, пикселей.
    pub height: u32,
    /// Пиксели RGBA.
    pub rgba: Vec<u8>,
}

impl Frame {
    /// Кадр из готовых байтов; длина обязана равняться `width * height * 3`.
    pub fn from_rgb(width: u32, height: u32, rgb: Vec<u8>) -> Result<Self, ScreenError> {
        let expected = width as usize * height as usize * 3;
        if rgb.len() != expected {
            return Err(ScreenError::BadFrame {
                expected,
                actual: rgb.len(),
            });
        }
        Ok(Self { width, height, rgb })
    }

    /// Однотонный кадр (основа синтетических кадров в тестах).
    pub fn filled(width: u32, height: u32, color: [u8; 3]) -> Self {
        let rgb = color
            .iter()
            .copied()
            .cycle()
            .take(width as usize * height as usize * 3)
            .collect();
        Self { width, height, rgb }
    }

    /// Ширина, пикселей.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Высота, пикселей.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Цвет пикселя; вне кадра — `None`.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 3]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let start = (y as usize * self.width as usize + x as usize) * 3;
        Some([self.rgb[start], self.rgb[start + 1], self.rgb[start + 2]])
    }

    /// Рисует поверх кадра изображение с прозрачностью (пиксели с alpha < 128 пропускаются).
    /// Нужен тестам: «браузер нарисовал значок на панели».
    pub fn paint(&mut self, image: &RgbaImage, left: u32, top: u32) {
        for y in 0..image.height {
            for x in 0..image.width {
                let start = (y as usize * image.width as usize + x as usize) * 4;
                if image.rgba[start + 3] < 128 {
                    continue;
                }
                let (target_x, target_y) = (left + x, top + y);
                if target_x >= self.width || target_y >= self.height {
                    continue;
                }
                let target = (target_y as usize * self.width as usize + target_x as usize) * 3;
                self.rgb[target..target + 3].copy_from_slice(&image.rgba[start..start + 3]);
            }
        }
    }

    /// Разбирает PNG снимка; ожидаемый размер известен заранее (экран стенда), и несовпадение
    /// длины данных — ошибка, а не молчаливо кривой кадр.
    pub fn decode_png(path: &Path, width: u32, height: u32) -> Result<Self, ScreenError> {
        let rgb = run_ffmpeg(path, "rgb24", None)?;
        Self::from_rgb(width, height, rgb)
    }
}

impl RgbaImage {
    /// Читает PNG значка и масштабирует до `size`x`size` (усреднением по площади: так
    /// браузер тоже уменьшает значок на панели, и края не «лесенкой»).
    pub fn decode_png_scaled(path: &Path, size: u32) -> Result<Self, ScreenError> {
        let rgba = run_ffmpeg(path, "rgba", Some(size))?;
        let expected = size as usize * size as usize * 4;
        if rgba.len() != expected {
            return Err(ScreenError::BadFrame {
                expected,
                actual: rgba.len(),
            });
        }
        Ok(Self {
            width: size,
            height: size,
            rgba,
        })
    }
}

/// Запускает `ffmpeg`, отдающий сырые пиксели `pixel_format`, при необходимости с масштабом
/// до квадрата `scale_to`.
fn run_ffmpeg(
    path: &Path,
    pixel_format: &str,
    scale_to: Option<u32>,
) -> Result<Vec<u8>, ScreenError> {
    let mut command = Command::new("ffmpeg");
    command.args(["-v", "error", "-nostdin", "-i"]).arg(path);
    if let Some(size) = scale_to {
        command.args(["-vf", &format!("scale={size}:{size}:flags=area")]);
    }
    command.args(["-f", "rawvideo", "-pix_fmt", pixel_format, "-"]);
    let output = command.output().map_err(|source| ScreenError::Spawn {
        program: "ffmpeg",
        source,
    })?;
    if !output.status.success() {
        return Err(ScreenError::Decode {
            path: path.to_path_buf(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(output.stdout)
}
