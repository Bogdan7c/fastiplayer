//! Перо-обёртка: масштабирует координаты из единиц шрифта в 26.6 способом FreeType.

use skrifa::outline::OutlinePen;

use super::FaceScale;

/// Пересчитывает координаты, приходящие от skrifa в единицах шрифта (`Size::unscaled`), в
/// пиксели размера 256 так, чтобы `pen.coordinate * 64` было ровно целым числом 26.6,
/// которое получил бы FreeType (`FT_MulFix(value, y_scale)`).
///
/// Целые координаты масштабируются без потерь. Полуцелые (неявные on-curve точки между
/// двумя квадратичными off-curve, которые skrifa считает как середину) FreeType считает
/// уже после масштабирования, поэтому здесь они могут отличаться на 1/64 пикселя —
/// это единственное известное отличие от FreeType и оно ниже порога видимости.
pub(super) struct FreeTypeScalePen<P: OutlinePen> {
    inner: P,
    scale: FaceScale,
}

impl<P: OutlinePen> FreeTypeScalePen<P> {
    pub(super) fn new(inner: P, scale: FaceScale) -> Self {
        Self { inner, scale }
    }

    /// Возвращает обёрнутое перо.
    pub(super) fn into_inner(self) -> P {
        self.inner
    }

    /// Единицы шрифта -> пиксели размера 256 (значение, кратное 1/64).
    fn convert(&self, units: f32) -> f32 {
        (self.scale.fractional_units_to_d6(f64::from(units)) as f32) / 64.0
    }
}

impl<P: OutlinePen> OutlinePen for FreeTypeScalePen<P> {
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = (self.convert(x), self.convert(y));
        self.inner.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = (self.convert(x), self.convert(y));
        self.inner.line_to(x, y);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (cx0, cy0) = (self.convert(cx0), self.convert(cy0));
        let (x, y) = (self.convert(x), self.convert(y));
        self.inner.quad_to(cx0, cy0, x, y);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (cx0, cy0) = (self.convert(cx0), self.convert(cy0));
        let (cx1, cy1) = (self.convert(cx1), self.convert(cy1));
        let (x, y) = (self.convert(x), self.convert(y));
        self.inner.curve_to(cx0, cy0, cx1, cy1, x, y);
    }

    fn close(&mut self) {
        self.inner.close();
    }
}
