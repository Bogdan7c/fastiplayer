//! Сверка пикселей показанных кадров (`--verify-pixels`, `--dump-dir`).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "стенд замера (пример, не production): отказ окружения прерывает прогон с \
              понятным сообщением"
)]

use gles2_egui::Gles2Context;

/// Сверка пикселей показанных кадров: ловит устаревшее содержимое текстур (кадр из кэша импортов
/// показывает не то, что декодер записал в буфер). Кадры движущегося видео должны давать почти
/// столько же разных хешей, сколько показано кадров.
#[derive(Default)]
pub struct PixelCheck {
    pub hashes: std::collections::HashSet<u64>,
    pub observed: usize,
    pub black_frames: usize,
    pub equal_to_previous: usize,
    previous_hash: Option<u64>,
}

/// Номера показанных кадров, которые сохраняются в PPM при `--dump-dir`.
const DUMPED_FRAME_INDICES: [usize; 3] = [40, 41, 300];

impl PixelCheck {
    pub fn observe(
        &mut self,
        context: &Gles2Context,
        shown_index: usize,
        dump_dir: Option<&std::path::Path>,
    ) {
        use std::hash::{Hash, Hasher};
        let pixels = context.read_offscreen_pixels().expect("offscreen readback");
        let rgba = pixels.as_rgba();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        rgba.hash(&mut hasher);
        let hash = hasher.finish();
        self.observed += 1;
        self.hashes.insert(hash);
        if rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[..3] == [0, 0, 0])
        {
            self.black_frames += 1;
        }
        if self.previous_hash == Some(hash) {
            self.equal_to_previous += 1;
        }
        self.previous_hash = Some(hash);
        if let (Some(dir), true) = (dump_dir, DUMPED_FRAME_INDICES.contains(&shown_index)) {
            let size = pixels.size();
            let mut ppm = format!("P6\n{} {}\n255\n", size.width(), size.height()).into_bytes();
            ppm.extend(
                rgba.as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]),
            );
            std::fs::write(dir.join(format!("frame-{shown_index}.ppm")), ppm).expect("write dump");
        }
    }
}
