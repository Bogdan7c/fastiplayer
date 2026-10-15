//! Раскладка масок субтитров по одной R8-текстуре (shelf packing).
//!
//! Модуль чисто CPU: он ничего не знает про wgpu и решает только геометрию. Вход — размеры
//! масок, выход — позиции в квадратном атласе. Алгоритм «полок»: маски сортируются по высоте
//! (от высоких к низким) и кладутся слева направо; когда строка кончилась, начинается новая
//! полка. Для набора из десятков плашек/глифов субтитров этого достаточно, а результат
//! детерминирован: один и тот же вход всегда даёт одну и ту же раскладку.
//!
//! Масок вплотную друг к другу не страшно: шейдер читает атлас через `textureLoad` по целым
//! координатам внутри размера маски, поэтому «протекания» соседей (как при билинейной
//! фильтрации) нет и отступы между масками не нужны.

/// Минимальная сторона атласа: меньше смысла нет, а пересоздавать ради мелочей дорого.
pub(super) const MIN_ATLAS_SIDE: u32 = 256;

/// Размер одной маски в пикселях (ширина и высота ненулевые — гарантирует растеризатор).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MaskExtent {
    pub(super) width: u32,
    pub(super) height: u32,
}

/// Левый верхний угол маски в атласе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AtlasSlot {
    pub(super) x: u32,
    pub(super) y: u32,
}

/// Результат раскладки: сторона квадратного атласа и слот каждой маски (в порядке входа).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AtlasLayout {
    pub(super) side: u32,
    pub(super) slots: Vec<AtlasSlot>,
}

/// Почему раскладка невозможна.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AtlasPackError {
    /// Маски не помещаются даже в атлас максимально допустимой стороны (лимит GPU).
    DoesNotFit { max_side: u32 },
}

/// Раскладывает маски, начиная с атласа `current_side` и удваивая сторону до `max_side`.
///
/// Атлас никогда не уменьшается (`current_side` — нижняя граница): так субтитры не
/// пересоздают текстуру туда-обратно, когда реплики то длинные, то короткие.
/// Возвращает минимальную подходящую степень двойки (или `max_side`, если он не степень).
pub(super) fn pack_masks(
    extents: &[MaskExtent],
    current_side: u32,
    max_side: u32,
) -> Result<AtlasLayout, AtlasPackError> {
    let mut side = current_side.max(MIN_ATLAS_SIDE).min(max_side);
    loop {
        if let Some(slots) = try_pack_into(extents, side) {
            return Ok(AtlasLayout { side, slots });
        }
        if side >= max_side {
            return Err(AtlasPackError::DoesNotFit { max_side });
        }
        // Удвоение с насыщением: `max_side` — потолок, даже если он не степень двойки.
        side = side.saturating_mul(2).min(max_side);
    }
}

/// Одна попытка уложить все маски в квадрат `side` x `side`; `None` — не влезло.
fn try_pack_into(extents: &[MaskExtent], side: u32) -> Option<Vec<AtlasSlot>> {
    // Быстрый отказ по площади: u64 исключает переполнение на больших масках.
    let total_area: u64 = extents
        .iter()
        .map(|extent| u64::from(extent.width) * u64::from(extent.height))
        .sum();
    if total_area > u64::from(side) * u64::from(side) {
        return None;
    }

    // Порядок укладки: высокие первыми; индекс как последний ключ делает порядок стабильным.
    let mut order: Vec<usize> = (0..extents.len()).collect();
    order.sort_by_key(|&index| {
        (
            std::cmp::Reverse(extents[index].height),
            std::cmp::Reverse(extents[index].width),
            index,
        )
    });

    let mut slots = vec![AtlasSlot { x: 0, y: 0 }; extents.len()];
    let mut cursor_x = 0_u32;
    let mut shelf_top = 0_u32;
    let mut shelf_height = 0_u32;
    for index in order {
        let extent = extents[index];
        if extent.width > side || extent.height > side {
            return None;
        }
        if cursor_x.checked_add(extent.width)? > side {
            // Полка кончилась: следующая начинается под самой высокой маской текущей.
            shelf_top = shelf_top.checked_add(shelf_height)?;
            cursor_x = 0;
            shelf_height = 0;
        }
        if shelf_top.checked_add(extent.height)? > side {
            return None;
        }
        slots[index] = AtlasSlot {
            x: cursor_x,
            y: shelf_top,
        };
        cursor_x += extent.width;
        // Первая маска полки самая высокая (сортировка), но max оставляем для надёжности.
        shelf_height = shelf_height.max(extent.height);
    }
    Some(slots)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extent(width: u32, height: u32) -> MaskExtent {
        MaskExtent { width, height }
    }

    /// Прямоугольники слотов не пересекаются и лежат внутри атласа.
    fn assert_valid(extents: &[MaskExtent], layout: &AtlasLayout) {
        assert_eq!(layout.slots.len(), extents.len());
        for (index, (slot, size)) in layout.slots.iter().zip(extents).enumerate() {
            assert!(
                slot.x + size.width <= layout.side,
                "mask {index} exits right"
            );
            assert!(
                slot.y + size.height <= layout.side,
                "mask {index} exits bottom"
            );
        }
        for first in 0..extents.len() {
            for second in first + 1..extents.len() {
                let (a, b) = (layout.slots[first], layout.slots[second]);
                let overlap_x =
                    a.x < b.x + extents[second].width && b.x < a.x + extents[first].width;
                let overlap_y =
                    a.y < b.y + extents[second].height && b.y < a.y + extents[first].height;
                assert!(
                    !(overlap_x && overlap_y),
                    "masks {first} and {second} overlap"
                );
            }
        }
    }

    #[test]
    fn small_set_fits_minimum_atlas_without_overlap() {
        let extents = [
            extent(100, 40),
            extent(30, 20),
            extent(120, 40),
            extent(60, 60),
        ];
        let layout = pack_masks(&extents, 0, 4096).expect("fits");
        assert_eq!(layout.side, MIN_ATLAS_SIDE);
        assert_valid(&extents, &layout);
    }

    #[test]
    fn packing_is_deterministic() {
        let extents = [extent(50, 10), extent(50, 10), extent(70, 30), extent(5, 5)];
        let first = pack_masks(&extents, 0, 4096).expect("fits");
        let second = pack_masks(&extents, 0, 4096).expect("fits");
        assert_eq!(first, second);
    }

    #[test]
    fn atlas_grows_by_doubling_until_masks_fit() {
        // Одна маска 300x300 не лезет в 256, но лезет в 512.
        let extents = [extent(300, 300), extent(200, 100)];
        let layout = pack_masks(&extents, 0, 4096).expect("fits after growth");
        assert_eq!(layout.side, 512);
        assert_valid(&extents, &layout);
    }

    #[test]
    fn atlas_never_shrinks_below_current_side() {
        let layout = pack_masks(&[extent(10, 10)], 1024, 4096).expect("fits");
        assert_eq!(layout.side, 1024);
    }

    #[test]
    fn masks_larger_than_gpu_limit_are_reported_not_panicked() {
        let error = pack_masks(&[extent(5000, 10)], 0, 4096).expect_err("too wide");
        assert_eq!(error, AtlasPackError::DoesNotFit { max_side: 4096 });
    }

    #[test]
    fn total_area_above_gpu_limit_is_reported() {
        // Каждая маска по отдельности влезает, но суммарно площадь больше 512x512.
        let extents = [extent(512, 300), extent(512, 300)];
        let error = pack_masks(&extents, 0, 512).expect_err("too much area");
        assert_eq!(error, AtlasPackError::DoesNotFit { max_side: 512 });
    }

    #[test]
    fn non_power_of_two_limit_is_used_as_ceiling() {
        let extents = [extent(600, 600)];
        let layout = pack_masks(&extents, 0, 700).expect("fits in the capped atlas");
        assert_eq!(layout.side, 700);
    }

    #[test]
    fn many_equal_masks_wrap_to_new_shelves() {
        let extents = vec![extent(100, 50); 9];
        let layout = pack_masks(&extents, 0, 4096).expect("fits");
        assert_valid(&extents, &layout);
        // Две маски на полку шириной 256 => девятая лежит на пятой полке.
        assert_eq!(layout.slots.iter().map(|slot| slot.y).max(), Some(200));
    }
}
