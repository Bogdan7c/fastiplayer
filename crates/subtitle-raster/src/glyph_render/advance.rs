//! Продвижение глифа «как у libass»: метрики FreeType после автохинтинга.
//!
//! libass при шейпинге просит у FreeType метрики глифов с флагами `FT_LOAD_DEFAULT` — даже
//! когда сам рисует контуры без хинтинга (`ass_glyph_metrics_construct`). Для шрифтов без
//! инструкций TrueType FreeType в этом режиме включает автохинтер, и продвижение глифа
//! округляется до целых пикселей размера 256 с учётом подгонки стеблей (например, `l` шириной
//! 43,48 px становится 44 px, а `o` шириной 101,97 px — 101 px). Накопленное за строку
//! расхождение с нехинтованными метриками достигает пикселя, поэтому для совпадения с libass
//! продвижения берутся из автохинтера skrifa (порт автохинтера FreeType, выбор движка тот же).
//!
//! Контуры при этом нехинтованные: хинтинг используется только для продвижений.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::cache::fast_hash::FastBuildHasher;

use skrifa::outline::{
    DrawSettings, HintingInstance, HintingOptions, OutlineGlyphCollection, OutlinePen,
};
use skrifa::{
    FontRef, GlyphId, MetadataProvider,
    instance::{LocationRef, Size},
};

use super::FaceScale;

/// Перо, отбрасывающее контур: нужно только скорректированное продвижение.
struct DiscardPen;

impl OutlinePen for DiscardPen {
    fn move_to(&mut self, _x: f32, _y: f32) {}
    fn line_to(&mut self, _x: f32, _y: f32) {}
    fn quad_to(&mut self, _cx0: f32, _cy0: f32, _x: f32, _y: f32) {}
    fn curve_to(&mut self, _cx0: f32, _cy0: f32, _cx1: f32, _cy1: f32, _x: f32, _y: f32) {}
    fn close(&mut self) {}
}

/// Хинтованные продвижения глифов одного лица при размере 256 px.
pub(crate) struct HintedAdvances {
    /// `None`, если хинтер не удалось создать: тогда используется линейное продвижение.
    instance: Option<HintingInstance>,
    /// Уже посчитанные продвижения (автохинтинг глифа не бесплатен).
    known: RefCell<HashMap<GlyphId, Option<i32>, FastBuildHasher>>,
}

impl HintedAdvances {
    /// Готовит хинтер лица для масштаба `scale` (FreeType: `FT_Request_Size` высотой 256 px).
    pub(crate) fn new(font: &FontRef<'_>, scale: &FaceScale) -> Self {
        let outlines = font.outline_glyphs();
        let instance = match HintingInstance::new(
            &outlines,
            Size::new(scale.hinting_pixels_per_em()),
            LocationRef::default(),
            HintingOptions::default(),
        ) {
            Ok(instance) => Some(instance),
            Err(error) => {
                tracing::debug!(%error, "cannot create a hinting instance, using linear advances");
                None
            }
        };
        Self {
            instance,
            known: RefCell::new(HashMap::default()),
        }
    }

    /// Продвижение, которое уже известно без обращения к контурам: `Some(None)` - хинтера нет
    /// или глиф не обработался (берётся линейное продвижение), `Some(Some(v))` - посчитано,
    /// `None` - ещё не считалось и нужны контуры ([`Self::advance`]).
    pub(crate) fn known_advance(&self, glyph: GlyphId) -> Option<Option<i32>> {
        if self.instance.is_none() {
            return Some(None);
        }
        self.known.borrow().get(&glyph).copied()
    }

    /// Хинтованное продвижение глифа в 26.6 при размере 256 px; `None` — хинтер недоступен
    /// или глиф не удалось обработать (вызывающий берёт линейное продвижение).
    pub(crate) fn advance(
        &self,
        outlines: &OutlineGlyphCollection<'_>,
        glyph: GlyphId,
    ) -> Option<i32> {
        let instance = self.instance.as_ref()?;
        if let Some(known) = self.known.borrow().get(&glyph) {
            return *known;
        }
        let computed = outlines.get(glyph).and_then(|outline| {
            outline
                .draw(DrawSettings::hinted(instance, false), &mut DiscardPen)
                .ok()
        });
        // FreeType округляет хинтованное продвижение до целого пикселя; skrifa делает то же.
        let advance = computed
            .and_then(|metrics| metrics.advance_width)
            .map(|pixels| (pixels * 64.0).round() as i32);
        self.known.borrow_mut().insert(glyph, advance);
        advance
    }
}
