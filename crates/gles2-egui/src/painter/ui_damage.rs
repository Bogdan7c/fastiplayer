//! Отпечаток интерфейса кадра: какие плитки цели что рисуют. Основа повреждения для частичного
//! показа.
//!
//! # Зачем плитки, а не границы мешей
//! egui склеивает все фигуры с одной текстурой и одним отсечением в ОДИН меш: панель внизу и
//! кнопка вверху лежат в одном меше, и его границы — почти весь экран. По границам меша любое
//! изменение интерфейса «повреждало» бы всю цель. Поэтому цель делится на плитки
//! ([`TILE_SIZE`] пикселей), и для каждой плитки считается хеш всех треугольников, которые её
//! касаются (в порядке рисования). Повреждение кадра — плитки, чей хеш отличается от хеша
//! прошлого ПОКАЗАННОГО кадра: неподвижная панель хеш не меняет, сдвинутая кнопка меняет плитки
//! на старом и новом месте.
//!
//! # Консервативность
//! Ошибка в сторону «изменилось» стоит лишних пикселей, ошибка в сторону «не изменилось» портит
//! картинку, поэтому: треугольник касается всех плиток своего ограничивающего прямоугольника
//! (не только реально закрытых); нерассчитываемые границы (NaN) = всё отсечение меша; смена
//! содержимого текстуры повреждает все треугольники с этой текстурой ([`TextureChange`]).

use epaint::{Mesh, TextureId};

use super::scissor::TopLeftPixelRect;
use crate::damage::{PixelRect, union_damage};

/// Сторона плитки в пикселях. 16 — компромисс: 1440×900 даёт ~5 тыс. плиток, большой
/// треугольник стоит ~5 тыс. умножений, а повреждение мельче сдвинутой кнопки не нужно.
pub(super) const TILE_SIZE: u32 = 16;

/// Что произошло с текстурой с прошлого собранного кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TextureChange {
    /// Текстура создана или целиком заменена.
    Replaced,
    /// Правка прямоугольника уже существующей текстуры.
    Patched,
}

/// Нужно ли считать правку изменением уже нарисованного (повредить все треугольники текстуры).
///
/// Атлас шрифтов egui (`TextureId::default()`) только дописывается: новый глиф получает новые
/// координаты, а их треугольники и так отличаются хешем, поэтому патч атласа ничего старого не
/// меняет. Любая другая текстура, а у атласа — полная замена, могут изменить пиксели под
/// неизменившимися треугольниками.
pub(super) fn damages_existing_pixels(texture: TextureId, change: TextureChange) -> bool {
    !(texture == TextureId::default() && change == TextureChange::Patched)
}

/// Отпечаток интерфейса одного собранного кадра.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct UiFootprint {
    target: [u32; 2],
    columns: u32,
    rows: u32,
    /// Хеш плитки; 0 — плитки не касается ни один треугольник.
    tiles: Vec<u64>,
    /// Площадь треугольников с изменившейся текстурой.
    texture_damage: Option<PixelRect>,
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv_extend(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

impl UiFootprint {
    /// Очистить под цель `target_size` (буфер плиток переиспользуется).
    pub(super) fn reset(&mut self, target_size: [u32; 2]) {
        self.target = target_size;
        self.columns = target_size[0].div_ceil(TILE_SIZE);
        self.rows = target_size[1].div_ceil(TILE_SIZE);
        self.tiles.clear();
        let tile_count = u64::from(self.columns) * u64::from(self.rows);
        self.tiles
            .resize(usize::try_from(tile_count).unwrap_or(0), 0);
        self.texture_damage = None;
    }

    /// Учесть меш (порядок вызовов = порядок рисования).
    ///
    /// `texture_damaged` — текстура меша изменилась так, что нарисованное ею могло поменяться.
    pub(super) fn add_mesh(
        &mut self,
        mesh: &Mesh,
        clip: TopLeftPixelRect,
        pixels_per_point: f32,
        texture_damaged: bool,
    ) {
        let clip_rect = PixelRect {
            x: clip.x,
            y: clip.y,
            width: clip.width,
            height: clip.height,
        };
        let clip_hash = fnv_extend(
            FNV_OFFSET,
            &[clip.x, clip.y, clip.width, clip.height]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<u8>>(),
        );
        let texture_hash = fnv_extend(clip_hash, &texture_bytes(mesh.texture_id));
        for triangle in mesh.indices.as_chunks::<3>().0 {
            let corners = [
                mesh.vertices.get(triangle[0] as usize),
                mesh.vertices.get(triangle[1] as usize),
                mesh.vertices.get(triangle[2] as usize),
            ];
            let [Some(first), Some(second), Some(third)] = corners else {
                // Невалидный индекс сюда не доходит (`Mesh::is_valid`); на всякий случай меш
                // целиком считается повреждённым.
                self.damage_whole_clip(clip_rect);
                continue;
            };
            let bounds =
                triangle_pixel_bounds([first.pos, second.pos, third.pos], pixels_per_point)
                    .and_then(|bounds| bounds.intersection(clip_rect));
            let Some(bounds) = bounds else {
                continue;
            };
            let mut hash = texture_hash;
            for vertex in [first, second, third] {
                hash = fnv_extend(hash, bytemuck::bytes_of(vertex));
            }
            self.mix_into_tiles(bounds, hash);
            if texture_damaged {
                self.texture_damage = union_damage(self.texture_damage, Some(bounds));
            }
        }
    }

    /// Невычислимый треугольник: меняется хеш всех плиток отсечения и всё отсечение повреждено.
    fn damage_whole_clip(&mut self, clip_rect: PixelRect) {
        self.mix_into_tiles(clip_rect, 0xDEAD_BEEF_DEAD_BEEF);
        self.texture_damage = union_damage(self.texture_damage, Some(clip_rect));
    }

    fn mix_into_tiles(&mut self, bounds: PixelRect, triangle_hash: u64) {
        let first_column = bounds.x / TILE_SIZE;
        let last_column = (bounds.x + bounds.width - 1) / TILE_SIZE;
        let first_row = bounds.y / TILE_SIZE;
        let last_row = (bounds.y + bounds.height - 1) / TILE_SIZE;
        for row in first_row..=last_row.min(self.rows.saturating_sub(1)) {
            for column in first_column..=last_column.min(self.columns.saturating_sub(1)) {
                let index = (row as usize) * (self.columns as usize) + column as usize;
                if let Some(tile) = self.tiles.get_mut(index) {
                    // Порядок рисования важен (смешивание не коммутативно): хеш цепочкой.
                    *tile = (*tile ^ triangle_hash).wrapping_mul(FNV_PRIME) | 1;
                }
            }
        }
    }

    /// Пиксели, которые этот отпечаток рисует иначе, чем `previous` (отпечаток последнего
    /// ПОКАЗАННОГО кадра). `None` в `previous` или другой размер цели — повреждена вся цель.
    /// Результат `None` — интерфейс не изменил ни одного пикселя.
    pub(crate) fn changed_since(&self, previous: Option<&Self>) -> Option<PixelRect> {
        let whole = PixelRect {
            x: 0,
            y: 0,
            width: self.target[0],
            height: self.target[1],
        };
        let Some(previous) = previous.filter(|previous| previous.target == self.target) else {
            return (!whole.is_empty()).then_some(whole);
        };
        let mut damage = self.texture_damage.or(None);
        for (index, (now, before)) in self.tiles.iter().zip(&previous.tiles).enumerate() {
            if now == before {
                continue;
            }
            let index = index as u32;
            let tile = PixelRect {
                x: (index % self.columns) * TILE_SIZE,
                y: (index / self.columns) * TILE_SIZE,
                width: TILE_SIZE,
                height: TILE_SIZE,
            };
            damage = union_damage(damage, tile.intersection(whole));
        }
        damage
    }
}

fn texture_bytes(texture: TextureId) -> [u8; 9] {
    let (tag, value) = match texture {
        TextureId::Managed(value) => (0, value),
        TextureId::User(value) => (1, value),
    };
    let mut bytes = [0; 9];
    bytes[0] = tag;
    bytes[1..].copy_from_slice(&value.to_le_bytes());
    bytes
}

/// Ограничивающий прямоугольник треугольника в пикселях цели, округлённый НАРУЖУ. Нерассчитываемые
/// координаты (NaN, бесконечность) — `None`: вызывающий считает треугольник невидимым только если
/// это безопасно; здесь NaN означает «рисовать нечего», как у растеризатора (NaN-вершины не
/// дают пикселей).
fn triangle_pixel_bounds(corners: [epaint::Pos2; 3], pixels_per_point: f32) -> Option<PixelRect> {
    if corners
        .iter()
        .any(|corner| !corner.x.is_finite() || !corner.y.is_finite())
    {
        return None;
    }
    let floor_pixel = |points: f32| (pixels_per_point * points).floor() as u32;
    let ceil_pixel = |points: f32| (pixels_per_point * points).ceil() as u32;
    let min_x = corners.iter().map(|corner| floor_pixel(corner.x)).min()?;
    let min_y = corners.iter().map(|corner| floor_pixel(corner.y)).min()?;
    let max_x = corners.iter().map(|corner| ceil_pixel(corner.x)).max()?;
    let max_y = corners.iter().map(|corner| ceil_pixel(corner.y)).max()?;
    // Каёма в 1 пиксель покрывает округление растеризации на границе.
    let rect = PixelRect {
        x: min_x.saturating_sub(1),
        y: min_y.saturating_sub(1),
        width: max_x.saturating_add(1) - min_x.saturating_sub(1),
        height: max_y.saturating_add(1) - min_y.saturating_sub(1),
    };
    (!rect.is_empty()).then_some(rect)
}

#[cfg(test)]
mod tests {
    use epaint::{Color32, Mesh, Rect, TextureId, pos2};

    use super::{TILE_SIZE, TextureChange, TopLeftPixelRect, UiFootprint, damages_existing_pixels};
    use crate::damage::PixelRect;

    const TARGET: [u32; 2] = [96, 64];

    fn whole_clip() -> TopLeftPixelRect {
        TopLeftPixelRect {
            x: 0,
            y: 0,
            width: TARGET[0],
            height: TARGET[1],
        }
    }

    fn quad(texture: TextureId, rect: Rect, color: Color32) -> Mesh {
        let mut mesh = Mesh::with_texture(texture);
        mesh.add_rect_with_uv(
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            color,
        );
        mesh
    }

    /// Отпечаток кадра из прямоугольников (все с белой текстурой, в порядке рисования).
    fn footprint_of(rects: &[(Rect, Color32)]) -> UiFootprint {
        let mut footprint = UiFootprint::default();
        footprint.reset(TARGET);
        for (rect, color) in rects {
            footprint.add_mesh(
                &quad(TextureId::default(), *rect, *color),
                whole_clip(),
                1.0,
                false,
            );
        }
        footprint
    }

    fn panel() -> (Rect, Color32) {
        (
            Rect::from_min_max(pos2(0.0, 50.0), pos2(96.0, 64.0)),
            Color32::GRAY,
        )
    }

    fn button(x: f32) -> (Rect, Color32) {
        (
            Rect::from_min_size(pos2(x, 8.0), epaint::vec2(10.0, 10.0)),
            Color32::RED,
        )
    }

    #[test]
    fn identical_frames_have_no_damage_and_first_frame_damages_everything() {
        let frame = footprint_of(&[panel(), button(10.0)]);
        assert_eq!(frame.changed_since(Some(&frame.clone())), None);
        assert_eq!(
            frame.changed_since(None),
            Some(PixelRect {
                x: 0,
                y: 0,
                width: 96,
                height: 64
            })
        );
    }

    #[test]
    fn moving_button_damages_old_and_new_place_but_not_the_static_panel() {
        let before = footprint_of(&[panel(), button(10.0)]);
        let after = footprint_of(&[panel(), button(60.0)]);
        let damage = after.changed_since(Some(&before)).expect("damage");
        // Старое (x 10..20) и новое (x 60..70) места внутри ограничивающего прямоугольника.
        assert!(
            damage.x <= 10 && damage.x + damage.width >= 70,
            "{damage:?}"
        );
        // Панель (y >= 50) не тронута: повреждение не заходит в её плитки.
        assert!(damage.y + damage.height <= 50, "{damage:?}");
        assert!(damage.height <= 2 * TILE_SIZE, "{damage:?}");
    }

    #[test]
    fn disappearing_element_is_damaged_where_it_was() {
        let before = footprint_of(&[panel(), button(10.0)]);
        let after = footprint_of(&[panel()]);
        let damage = after.changed_since(Some(&before)).expect("damage");
        assert!(
            damage.x <= 10 && damage.x + damage.width >= 20,
            "{damage:?}"
        );
    }

    #[test]
    fn drawing_order_matters() {
        let red_over_blue = footprint_of(&[
            (
                Rect::from_min_max(pos2(0.0, 0.0), pos2(20.0, 20.0)),
                Color32::RED,
            ),
            (
                Rect::from_min_max(pos2(0.0, 0.0), pos2(20.0, 20.0)),
                Color32::BLUE,
            ),
        ]);
        let blue_over_red = footprint_of(&[
            (
                Rect::from_min_max(pos2(0.0, 0.0), pos2(20.0, 20.0)),
                Color32::BLUE,
            ),
            (
                Rect::from_min_max(pos2(0.0, 0.0), pos2(20.0, 20.0)),
                Color32::RED,
            ),
        ]);
        assert!(red_over_blue.changed_since(Some(&blue_over_red)).is_some());
    }

    #[test]
    fn replaced_texture_damages_its_triangles_even_when_geometry_is_unchanged() {
        let user_texture = TextureId::User(5);
        let build = |texture_damaged: bool| {
            let mut footprint = UiFootprint::default();
            footprint.reset(TARGET);
            footprint.add_mesh(
                &quad(
                    user_texture,
                    Rect::from_min_max(pos2(20.0, 20.0), pos2(40.0, 40.0)),
                    Color32::WHITE,
                ),
                whole_clip(),
                1.0,
                texture_damaged,
            );
            footprint
        };
        let before = build(false);
        assert_eq!(build(false).changed_since(Some(&before)), None);
        let damage = build(true).changed_since(Some(&before)).expect("damage");
        assert!(
            damage.x <= 20 && damage.x + damage.width >= 40,
            "{damage:?}"
        );
    }

    #[test]
    fn font_atlas_patch_is_harmless_but_other_texture_changes_are_not() {
        let font = TextureId::default();
        assert!(!damages_existing_pixels(font, TextureChange::Patched));
        assert!(damages_existing_pixels(font, TextureChange::Replaced));
        assert!(damages_existing_pixels(
            TextureId::User(1),
            TextureChange::Patched
        ));
        assert!(damages_existing_pixels(
            TextureId::Managed(7),
            TextureChange::Patched
        ));
    }

    #[test]
    fn different_target_size_damages_everything() {
        let small = footprint_of(&[panel()]);
        let mut other = UiFootprint::default();
        other.reset([128, 64]);
        assert_eq!(
            other.changed_since(Some(&small)),
            Some(PixelRect {
                x: 0,
                y: 0,
                width: 128,
                height: 64
            })
        );
    }
}
