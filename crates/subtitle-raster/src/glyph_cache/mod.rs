// Модель кэшей: libass 0.17.5, ass_cache.c / ass_cache_template.h (хэш + LRU + счётчики
// ссылок) и ass_render.c (get_outline_glyph, get_bitmap_glyph, render_and_combine_glyphs,
// ass_composite_construct, check_cache_limits).
//
// Copyright (C) 2006 Evgeniy Stepanov <eugeni.stepanov@gmail.com>
//
// Permission to use, copy, modify, and distribute this software for any
// purpose with or without fee is hereby granted, provided that the above
// copyright notice and this permission notice appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
// WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
// MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
// ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
// ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
// OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
//
// Rust port © Fastiplayer contributors.

//! Трёхуровневый кэш глифов, как в libass.
//!
//! Кэш реплик (`SubtitleRasterizer::cue_cache`) спасает только статичную реплику. Движущаяся,
//! затухающая или караоке-реплика на каждом кадре получает новый ключ и рисуется с нуля,
//! хотя почти вся работа (контуры, обводка, растеризация букв, склейка) на соседних кадрах
//! одна и та же. Поэтому ниже кэша реплик лежат три кэша, каждый кормит следующий:
//!
//! 1. **Контуры** ([`OutlineCache`], libass `outline_cache`, считаются в ШТУКАХ): контуры
//!    глифов шрифта (с синтетическими жирностью и наклоном и линиями), разобранные рисунки
//!    `\p`, обводки контуров (результат обводчика) и плашка `BorderStyle = 3`. Неудачи
//!    (глиф не загрузился, обводка не получилась, рисунок отвергнут) хранятся как записи.
//! 2. **Маски глифов** ([`BitmapCache`], libass `bitmap_cache`, в БАЙТАХ): ключ - контур
//!    (его идентичность) и квантованное преобразование `QuantizedTransform` БЕЗ целой части
//!    позиции. Сдвиг глифа на целые пиксели (`\move` на целое число пикселей, прокрутка
//!    Banner/Scroll) даёт тот же ключ: маска растеризуется один раз. Растеризация всегда идёт
//!    из квантованного ключа, поэтому результат побитно тот же, что без кэша.
//! 3. **Склейки** ([`CompositeCache`], libass `composite_cache`, в БАЙТАХ): ключ - описание
//!    фильтров и список частей участка (идентичности масок глифов и их целые позиции
//!    ОТНОСИТЕЛЬНО начала участка). Значение - готовые маски заливки, обводки и тени после
//!    сложения, размытия и сдвига тени. Цвета, клип и караоке в ключ не входят: они
//!    применяются после кэша (как в libass).
//!
//! ## Идентичность по указателю
//! Ключи верхних кэшей хранят `Arc` значений нижних ([`ArcId`]) и сравниваются по адресу
//! ([`Arc::ptr_eq`]), а не по содержимому. Это безопасно, потому что ключ держит сильную
//! ссылку: пока ключ жив, адрес не может достаться другому значению, значит равные адреса
//! всегда означают одно и то же значение. Цена та же, что в libass: вытесненное из своего кэша
//! значение остаётся в памяти, пока на него ссылается чей-то ключ, поэтому настоящий расход
//! памяти может ненадолго превысить сумму лимитов.
//!
//! ## Вытеснение
//! Как `check_cache_limits` в libass: записи вставляются без вытеснения, а лимиты
//! применяются [`GlyphCaches::cut_to_limits`] в НАЧАЛЕ каждого кадра (`SubtitleRasterizer::render`)
//! в порядке склейки, маски, контуры. Так рабочий набор одного кадра не вытесняет сам себя.
//! Движок LRU - `crate::cache::ByteLruCache` (`insert_deferred` + `cut`).
//!
//! ## Владение и сброс
//! Все три кэша принадлежат `SubtitleRasterizer` и используются из одного потока отрисовки
//! (блокировок нет). Идентификаторы лиц ([`FaceId`](crate::fonts::FaceId)) не переиспользуются
//! внутри библиотеки шрифтов, но при ЛЮБОМ изменении набора шрифтов (вложенные, системные)
//! кэши сбрасываются целиком вместе с кэшами лиц и реплик ([`GlyphCaches::clear`]): выбор
//! лица для тех же символов мог измениться.
//!
//! ## Что не кэшируется
//! Ошибки растеризации и композиции, кроме штатных отказов по размеру (`ExceedsSideLimit`),
//! в кэш не попадают. Маска векторного клипа `\clip(...)` кэшем не пользуется (libass её кэширует,
//! но она считается один раз на реплику и мала).

mod bitmap;
mod composite;
mod identity;
mod outline;

pub(crate) use bitmap::{BitmapCache, BitmapSource};
pub(crate) use composite::{
    CompositeCache, CompositeEntry, CompositeKey, FilterKey, PartKey, PlacedMask, SharedMask,
};
pub(crate) use identity::ArcId;
pub(crate) use outline::{DrawingEntry, DrawingOutline, OutlineCache};

use crate::cache::CacheStatistics;

/// Лимиты трёх кэшей (из `RasterCacheLimits`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GlyphCacheLimits {
    /// Сколько записей держать в кэше контуров.
    pub(crate) outline_items: usize,
    /// Сколько байт масок глифов держать.
    pub(crate) bitmap_bytes: usize,
    /// Сколько байт склеек держать.
    pub(crate) composite_bytes: usize,
}

/// Снимок счётчиков трёх кэшей.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GlyphCacheStatistics {
    /// Кэш контуров (его «байты» - штуки).
    pub(crate) outlines: CacheStatistics,
    /// Кэш масок глифов.
    pub(crate) bitmaps: CacheStatistics,
    /// Кэш склеек.
    pub(crate) composites: CacheStatistics,
}

/// Три кэша глифов одного растеризатора.
pub(crate) struct GlyphCaches {
    pub(crate) outlines: OutlineCache,
    pub(crate) bitmaps: BitmapCache,
    pub(crate) composites: CompositeCache,
}

impl GlyphCaches {
    /// Пустые кэши с заданными лимитами.
    pub(crate) fn new(limits: GlyphCacheLimits) -> Self {
        Self {
            outlines: OutlineCache::new(limits.outline_items),
            bitmaps: BitmapCache::new(limits.bitmap_bytes),
            composites: CompositeCache::new(limits.composite_bytes),
        }
    }

    /// Кэши без ограничений на время одной реплики: для путей, у которых нет своего
    /// растеризатора (простые форматы, тесты). Ничего не вытесняется, потому что `cut` не
    /// вызывается, а память живёт ровно столько, сколько сам набор.
    pub(crate) fn transient() -> Self {
        Self::new(GlyphCacheLimits {
            outline_items: usize::MAX,
            bitmap_bytes: usize::MAX,
            composite_bytes: usize::MAX,
        })
    }

    /// Применяет лимиты: склейки, затем маски, затем контуры (libass: `check_cache_limits`).
    pub(crate) fn cut_to_limits(&mut self) {
        self.composites.cut();
        self.bitmaps.cut();
        self.outlines.cut();
    }

    /// Забывает всё (набор шрифтов изменился). Счётчики сохраняются.
    pub(crate) fn clear(&mut self) {
        self.composites.clear();
        self.bitmaps.clear();
        self.outlines.clear();
    }

    /// Счётчики всех трёх кэшей.
    pub(crate) fn statistics(&self) -> GlyphCacheStatistics {
        GlyphCacheStatistics {
            outlines: self.outlines.statistics(),
            bitmaps: self.bitmaps.statistics(),
            composites: self.composites.statistics(),
        }
    }
}

#[cfg(test)]
mod tests;
