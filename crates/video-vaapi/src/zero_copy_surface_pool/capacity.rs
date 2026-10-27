//! Ёмкость import-слотов `ZeroCopySurfacePool` и её связь с пулом выходных VA surfaces.
//!
//! Инвариант: каждая живая VA surface регистрируется (импортируется) максимум в
//! одном слоте, поэтому число слотов не может быть меньше действующей ёмкости
//! пула выходных surfaces. Пул surfaces сам поднимает ёмкость до минимума стрима
//! (минимум кодека + запас внешних держателей), и слоты обязаны за ним следовать.
//! Иначе при `slots < live surfaces` регистрация упирается в `PoolExhausted`, а
//! декодер падает фатально, хотя настоящей утечки нет.
//!
//! Настройка `zero_copy_surface_pool_slots` — нижняя граница: слоты растут вслед за
//! пулом surfaces, но не уменьшаются ниже настройки и ниже числа уже занятых слотов.

use super::ZeroCopySurfacePool;

/// Итог согласования ёмкости слотов с ёмкостью пула выходных surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZeroCopyCapacityChange {
    /// Слотов уже достаточно: ёмкость не изменилась.
    Unchanged {
        /// Действующая ёмкость слотов.
        capacity: usize,
    },

    /// Ёмкость слотов поднята вслед за пулом выходных surfaces.
    Raised {
        /// Ёмкость до подъёма (настройка пользователя или прошлый подъём).
        previous: usize,

        /// Ёмкость после подъёма.
        current: usize,
    },
}

impl ZeroCopySurfacePool {
    /// Поднимает ёмкость слотов минимум до `surface_pool_capacity`.
    ///
    /// Только рост: уменьшение ёмкости могло бы отрезать уже занятые слоты, а
    /// лишний запас стоит дёшево (слот занимает память только после реального
    /// импорта). Занятые слоты и их состояния не затрагиваются.
    pub fn raise_capacity_to_follow_surfaces(
        &mut self,
        surface_pool_capacity: usize,
    ) -> ZeroCopyCapacityChange {
        let previous = self.capacity;
        if surface_pool_capacity <= previous {
            return ZeroCopyCapacityChange::Unchanged { capacity: previous };
        }
        self.capacity = surface_pool_capacity;
        ZeroCopyCapacityChange::Raised {
            previous,
            current: surface_pool_capacity,
        }
    }
}

#[cfg(test)]
mod tests;
