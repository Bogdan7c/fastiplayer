//! Выходной контракт растеризатора: список картинок поверх кадра.
//!
//! Модель повторяет `ASS_Image` из libass 0.17.5 (`ass.h`): картинка текста — это 8-битная
//! маска покрытия, один цвет RGBA и позиция. Рендер (GPU сейчас, GLES2 потом) не знает ничего
//! про шрифты и ASS: он только заливает цвет через маску в порядке списка. Для ТЕКСТА
//! альтернатива «готовая RGBA-картинка» отвергнута: маска в 4 раза компактнее, цвет можно
//! менять без повторной растеризации, а одинаковые маски делятся между кадрами через `Arc`.
//!
//! Контракт расширен осознанно для авторских растровых субтитров (PGS): их цвета заданы
//! палитрой автора и не сводятся к одному цвету на маску, поэтому такая картинка несёт
//! полноцветные RGBA-пиксели с ПРЕМУЛЬТИПЛИЦИРОВАННОЙ альфой ([`BitmapPixelFormat`],
//! [`SubtitleBitmapKind::Picture`], см. `overlay/picture.rs`). Формат пикселей картинки
//! задаёт [`SubtitleBitmap::pixel_format`]; потребитель обязан его проверять, а не читать байты
//! вслепую.

use std::num::NonZeroU32;
use std::sync::Arc;

use subtitle_core::{PictureForcedFlag, SubtitleColor};

use crate::renderer::CueKey;

mod animation_change;
mod crop;
mod mask_view;
mod picture;
mod placement_group;

pub use animation_change::AnimationChange;
pub(crate) use crop::LocalRect;
pub use picture::BitmapPixelFormat;
pub use placement_group::OverlayPlacementGroup;
use placement_group::union_bounds;

/// Размер холста в пикселях.
///
/// Холст равен отображаемой картинке видео: координаты всех картинок отсчитываются от её
/// левого верхнего угла, а не от окна (чёрные полосы в холст не входят).
///
/// Верхней границы у самого типа нет (достаточно `u32`), поэтому защита от гигантских холстов
/// лежит на растеризаторе: `RasterInputLimits::max_canvas_side` — холст больше лимита по любой
/// стороне получает пустой результат, а внутренняя арифметика (26.6, размеры масок) работает
/// только с холстами в пределах лимита.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CanvasSize {
    width: NonZeroU32,
    height: NonZeroU32,
}

impl CanvasSize {
    /// Создаёт размер холста; нулевая сторона недопустима (`None`).
    pub fn new(width: u32, height: u32) -> Option<Self> {
        Some(Self {
            width: NonZeroU32::new(width)?,
            height: NonZeroU32::new(height)?,
        })
    }

    /// Ширина холста в пикселях.
    pub fn width(&self) -> u32 {
        self.width.get()
    }

    /// Высота холста в пикселях.
    pub fn height(&self) -> u32 {
        self.height.get()
    }
}

/// Точка в пикселях холста (левый верхний угол картинки).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelPoint {
    /// Горизонталь, пиксели слева направо.
    pub x: i32,
    /// Вертикаль, пиксели сверху вниз.
    pub y: i32,
}

/// Прямоугольник в пикселях холста: левый верхний угол и размер.
///
/// Нужен вызывающему коду, чтобы знать, где на холсте лежит видимое содержимое
/// ([`SubtitleOverlay::content_bounds`]), не разбирая отдельные картинки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CanvasRect {
    /// Левый край, пиксели слева направо.
    pub x: i32,
    /// Верхний край, пиксели сверху вниз.
    pub y: i32,
    /// Ширина в пикселях.
    pub width: u32,
    /// Высота в пикселях.
    pub height: u32,
}

/// Что изображено в картинке. Порядок отрисовки задаётся порядком в списке,
/// `kind` нужен диагностике и будущим эффектам (например, размытию только обводки).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubtitleBitmapKind {
    /// Плашка под текстом.
    Background,
    /// Тень.
    Shadow,
    /// Обводка.
    Outline,
    /// Сами глифы.
    Glyph,
    /// Авторская растровая картинка (PGS): полноцветная, формат пикселей
    /// [`BitmapPixelFormat::PremultipliedRgba`].
    Picture,
}

/// Почему картинку нельзя построить (нарушен инвариант раскладки маски).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SubtitleBitmapError {
    /// Ширина или высота равна нулю.
    #[error("bitmap has an empty area: {width}x{height}")]
    EmptyArea {
        /// Заявленная ширина.
        width: u32,
        /// Заявленная высота.
        height: u32,
    },
    /// Шаг строки меньше ширины.
    #[error("bitmap stride {stride} is smaller than width {width}")]
    StrideSmallerThanWidth {
        /// Заявленный шаг строки.
        stride: usize,
        /// Заявленная ширина.
        width: u32,
    },
    /// Маска короче, чем нужно для `height` строк.
    #[error("coverage has {actual} bytes but {required} are required")]
    CoverageTooShort {
        /// Фактическая длина маски.
        actual: usize,
        /// Требуемая длина.
        required: usize,
    },
}

/// Одна картинка: пиксели + цвет + позиция.
///
/// Для маски покрытия ([`BitmapPixelFormat::Coverage`]) итоговая прозрачность пикселя =
/// `coverage * color.alpha / 255`. Цвет прямой (не инвертированный, в отличие от
/// `ASS_Image.color` в libass, где альфа хранится как 255 - a).
///
/// Для RGBA-картинки ([`BitmapPixelFormat::PremultipliedRgba`]) цвет работает как множитель
/// непрозрачности: берётся только `color.alpha`, контракт описан в модуле `overlay/picture.rs`.
#[derive(Debug, Clone)]
pub struct SubtitleBitmap {
    origin: PixelPoint,
    width: u32,
    height: u32,
    stride: usize,
    /// Общий буфер пикселей (маска покрытия или RGBA, см. `format`). Картинка показывает
    /// только его окно, см. `mask_view`.
    coverage: Arc<[u8]>,
    /// Смещение первого видимого байта окна в `coverage`.
    coverage_offset: usize,
    color: SubtitleColor,
    kind: SubtitleBitmapKind,
    /// Формат байтов буфера; `stride` и окно всегда считаются в байтах этого формата.
    format: BitmapPixelFormat,
    /// Пометка forced объекта-картинки; у масок текста всегда `Regular`.
    forced: PictureForcedFlag,
    placement_group: OverlayPlacementGroup,
    /// Реплика, которой принадлежит картинка. `None` только у картинок, собранных не
    /// растеризатором реплик (например, в юнит-тестах отдельных этапов): растеризатор
    /// проставляет владельца каждой картинке кадра ([`Self::with_owner`]).
    owner: Option<CueKey>,
}

impl SubtitleBitmap {
    /// Строит картинку, проверяя раскладку маски.
    ///
    /// Инвариант «картинка целиком внутри холста» обеспечивает растеризатор обрезкой и
    /// проверяет [`SubtitleOverlay::new`]; здесь холст неизвестен.
    pub(crate) fn new(
        origin: PixelPoint,
        width: u32,
        height: u32,
        stride: usize,
        coverage: Arc<[u8]>,
        color: SubtitleColor,
        kind: SubtitleBitmapKind,
    ) -> Result<Self, SubtitleBitmapError> {
        Self::from_shared_view(origin, width, height, stride, coverage, 0, color, kind)
    }

    /// Та же картинка (та же маска), отнесённая к реплике `owner`.
    pub(crate) fn with_owner(self, owner: CueKey) -> Self {
        Self {
            owner: Some(owner),
            ..self
        }
    }

    /// Та же картинка, отнесённая к группе размещения `group`.
    pub(crate) fn with_placement_group(self, group: OverlayPlacementGroup) -> Self {
        Self {
            placement_group: group,
            ..self
        }
    }

    /// Та же картинка (та же маска, тот же `Arc`), сдвинутая по вертикали на `delta` пикселей.
    ///
    /// Нужна разрешению столкновений реплик: сдвиг меняет только позицию, поэтому
    /// [`detect_overlay_change`] увидит [`OverlayChange::Moved`], а не `Replaced`.
    /// Проверку «внутри холста» делает [`SubtitleOverlay::new`].
    pub(crate) fn shifted_vertically(&self, delta: i32) -> Self {
        Self {
            origin: PixelPoint {
                x: self.origin.x,
                y: self.origin.y.saturating_add(delta),
            },
            ..self.clone()
        }
    }

    /// Картинка, сдвинутая по вертикали на `delta` пикселей с обрезкой по холсту высотой
    /// `canvas_height` (libass: `shift_event`). `None` — после обрезки ничего не осталось.
    ///
    /// Без обрезки маска общая (как у [`Self::shifted_vertically`]); с обрезкой строки
    /// копируются заново.
    pub(crate) fn shifted_vertically_clipped(
        &self,
        delta: i32,
        canvas_height: u32,
    ) -> Option<Self> {
        let top = i64::from(self.origin.y) + i64::from(delta);
        let bottom = top + i64::from(self.height);
        let clip_top = (-top).max(0);
        let clip_bottom = (bottom - i64::from(canvas_height)).max(0);
        let kept_rows = i64::from(self.height) - clip_top - clip_bottom;
        if kept_rows <= 0 {
            return None;
        }
        if clip_top == 0 && clip_bottom == 0 {
            return Some(self.shifted_vertically(delta));
        }
        let skipped_bytes = usize::try_from(clip_top).ok()?.checked_mul(self.stride)?;
        // Оставляем только видимые строки: обрезанный низ не должен жить в `Arc` кадра.
        let kept_end = usize::try_from(clip_top + kept_rows)
            .ok()?
            .checked_mul(self.stride)?
            .min(self.coverage_window().len());
        let visible = self.coverage_window().get(skipped_bytes..kept_end)?;
        Some(Self {
            coverage_offset: 0,
            origin: PixelPoint {
                x: self.origin.x,
                y: i32::try_from(top + clip_top).ok()?,
            },
            height: u32::try_from(kept_rows).ok()?,
            coverage: Arc::from(visible),
            ..self.clone()
        })
    }

    /// Левый верхний угол в пикселях холста.
    pub fn origin(&self) -> PixelPoint {
        self.origin
    }

    /// Ширина в пикселях.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Высота в пикселях.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Шаг строки буфера в БАЙТАХ (не меньше `width * bytes_per_pixel`): для маски покрытия
    /// не меньше ширины, для RGBA не меньше `4 * width`.
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// Маска покрытия 0..=255, строки идут подряд с шагом [`Self::stride`]. Первый байт -
    /// левый верхний пиксель картинки; картинка у края кадра может быть окном в большей
    /// общей маске, и тогда за последней строкой данных нет (хвоста шага тоже).
    ///
    /// Только для [`BitmapPixelFormat::Coverage`]: у RGBA-картинки маски нет, и метод
    /// возвращает ПУСТОЙ срез, чтобы цветные байты нельзя было принять за покрытие.
    /// Пиксели RGBA-картинки отдаёт [`Self::rgba_pixels`].
    pub fn coverage(&self) -> &[u8] {
        match self.format {
            BitmapPixelFormat::Coverage => self.coverage_window(),
            BitmapPixelFormat::PremultipliedRgba => &[],
        }
    }

    /// Окно пикселей картинки в БАЙТАХ её формата (для любого формата): сколько памяти
    /// занимает видимая часть. Нужен учёту кэшей и статистике.
    pub fn pixel_bytes(&self) -> usize {
        self.coverage_window().len()
    }

    /// Весь общий буфер, в котором лежит маска (может быть больше окна картинки). Для
    /// сравнения картинок на «те же пиксели» используйте [`Self::shows_same_pixels_as`]: один
    /// буфер может показываться разными окнами.
    pub fn shared_coverage(&self) -> &Arc<[u8]> {
        &self.coverage
    }

    /// Цвет слоя (прямой RGBA).
    pub fn color(&self) -> SubtitleColor {
        self.color
    }

    /// Вид слоя.
    pub fn kind(&self) -> SubtitleBitmapKind {
        self.kind
    }

    /// Группа размещения: какой частью слоя двигает показ (основные реплики или фиксированные).
    pub fn placement_group(&self) -> OverlayPlacementGroup {
        self.placement_group
    }

    /// Реплика, которой принадлежит картинка: по ней вызывающий код отличает картинки
    /// закончившейся реплики от оставшихся ([`SubtitleOverlay::retain_cues`]).
    /// `None` бывает только у картинок, собранных не растеризатором реплик.
    pub fn cue_key(&self) -> Option<CueKey> {
        self.owner
    }

    /// Строка маски `row` (ровно `width` байт) или `None` за пределами картинки либо у
    /// RGBA-картинки (маски нет).
    pub fn coverage_row(&self, row: u32) -> Option<&[u8]> {
        if row >= self.height || self.format != BitmapPixelFormat::Coverage {
            return None;
        }
        let start = (row as usize).checked_mul(self.stride)?;
        let end = start.checked_add(self.width as usize)?;
        self.coverage_window().get(start..end)
    }
}

/// Что изменилось относительно предыдущего результата рендера
/// (libass 0.17.5: `ass_render_frame` -> `detect_change`: 0 / 1 / 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OverlayChange {
    /// Картинки те же и на тех же местах: текстуры перезагружать не нужно.
    Unchanged,
    /// Картинки те же, но часть сдвинулась: достаточно обновить позиции.
    Moved,
    /// Содержимое новое: текстуры нужно загрузить заново.
    Replaced,
}

/// Номер результата рендера; растёт при каждом новом результате.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OverlayRevision(u64);

impl OverlayRevision {
    /// Начальная ревизия (до первого рендера).
    pub const INITIAL: Self = Self(0);

    /// Численное значение.
    pub fn value(&self) -> u64 {
        self.0
    }

    /// Следующая ревизия; на переполнении u64 (недостижимо) остаётся на месте.
    pub(crate) fn next(&self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Нарушение инварианта «картинка целиком внутри холста».
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("bitmap #{bitmap_index} does not fit into the {canvas_width}x{canvas_height} canvas")]
pub struct BitmapOutsideCanvas {
    /// Номер картинки в списке.
    pub bitmap_index: usize,
    /// Ширина холста.
    pub canvas_width: u32,
    /// Высота холста.
    pub canvas_height: u32,
}

/// Результат растеризации: холст, картинки по порядку отрисовки, ревизия и признак изменения.
#[derive(Debug, Clone)]
pub struct SubtitleOverlay {
    canvas: CanvasSize,
    bitmaps: Vec<SubtitleBitmap>,
    revision: OverlayRevision,
    change: OverlayChange,
    /// Когда анимация реплик изменит картинку (см. [`AnimationChange`]).
    animation_change: AnimationChange,
}

impl SubtitleOverlay {
    /// Собирает результат, проверяя, что каждая картинка лежит внутри холста.
    pub(crate) fn new(
        canvas: CanvasSize,
        bitmaps: Vec<SubtitleBitmap>,
        revision: OverlayRevision,
        change: OverlayChange,
    ) -> Result<Self, BitmapOutsideCanvas> {
        for (bitmap_index, bitmap) in bitmaps.iter().enumerate() {
            if !fits_inside(canvas, bitmap) {
                return Err(BitmapOutsideCanvas {
                    bitmap_index,
                    canvas_width: canvas.width(),
                    canvas_height: canvas.height(),
                });
            }
        }
        Ok(Self {
            canvas,
            bitmaps,
            revision,
            change,
            animation_change: AnimationChange::None,
        })
    }

    /// Пустой результат (нет ни одной картинки).
    pub(crate) fn empty(
        canvas: CanvasSize,
        revision: OverlayRevision,
        change: OverlayChange,
    ) -> Self {
        Self {
            canvas,
            bitmaps: Vec::new(),
            revision,
            change,
            animation_change: AnimationChange::None,
        }
    }

    /// Записывает, когда анимация изменит картинку (растеризатор знает это при сборке кадра).
    pub(crate) fn with_animation_change(mut self, animation_change: AnimationChange) -> Self {
        self.animation_change = animation_change;
        self
    }

    /// Помечает результат как повторно выданный без изменений (ревизия остаётся прежней).
    pub(crate) fn mark_unchanged(&mut self) {
        self.change = OverlayChange::Unchanged;
    }

    /// Новый результат только с картинками реплик, которые `keep` оставляет.
    ///
    /// Нужен показу, когда точной картинки для кадра ещё нет: берём последнюю показанную,
    /// но без реплик, которые уже закончились (их картинки знают свою реплику).
    ///
    /// Семантика:
    /// - маски общие (`Arc::ptr_eq` с исходными), порядок отрисовки сохраняется;
    /// - ничего не отброшено — результат равен исходному (та же ревизия, тот же `change`);
    /// - что-то отброшено — ревизия остаётся исходной, а `change` = [`OverlayChange::Replaced`]:
    ///   новые ревизии выдаёт только растеризатор (счётчик у него), а GPU-оболочка опознаёт
    ///   загруженное по паре «ревизия + `Arc` каждой маски», и другой набор масок она
    ///   отличит сама;
    /// - картинки без владельца ([`SubtitleBitmap::cue_key`] = `None`) не отбрасываются:
    ///   решить, нужны ли они, нельзя;
    /// - состояние растеризатора не затрагивается (метод работает только с самим результатом).
    pub fn retain_cues(&self, keep: impl Fn(CueKey) -> bool) -> SubtitleOverlay {
        let kept: Vec<SubtitleBitmap> = self
            .bitmaps
            .iter()
            .filter(|bitmap| bitmap.owner.is_none_or(&keep))
            .cloned()
            .collect();
        if kept.len() == self.bitmaps.len() {
            return self.clone();
        }
        Self {
            canvas: self.canvas,
            bitmaps: kept,
            revision: self.revision,
            change: OverlayChange::Replaced,
            // Консервативно: оставшиеся реплики меняются не позже, чем весь набор.
            animation_change: self.animation_change,
        }
    }

    /// Размер холста.
    pub fn canvas(&self) -> CanvasSize {
        self.canvas
    }

    /// Картинки в порядке отрисовки (первая — самая нижняя).
    pub fn bitmaps(&self) -> &[SubtitleBitmap] {
        &self.bitmaps
    }

    /// Ревизия результата.
    pub fn revision(&self) -> OverlayRevision {
        self.revision
    }

    /// Что изменилось относительно предыдущего результата.
    pub fn change(&self) -> OverlayChange {
        self.change
    }

    /// Когда картинка изменится сама по себе (анимации `\move`, `\fad`), если запрос
    /// повторить в более поздний момент. [`AnimationChange::None`] — у статичной сцены.
    ///
    /// Значение верно для моментов не раньше момента запроса, по которому кадр собран. Оно же
    /// удобно для границы срока годности картинки ([`AnimationChange::holds_at`]).
    pub fn animation_change(&self) -> AnimationChange {
        self.animation_change
    }

    /// Нет ни одной картинки.
    pub fn is_empty(&self) -> bool {
        self.bitmaps.is_empty()
    }

    /// Объединяющий прямоугольник всех картинок в пикселях холста; `None`, если картинок нет.
    ///
    /// Границы берутся по размерам картинок целиком (включая нулевое покрытие внутри маски):
    /// это стабильная геометрия слоя, которая не «дышит» от содержимого масок. Инвариант
    /// «внутри холста» гарантирует, что результат тоже внутри холста.
    pub fn content_bounds(&self) -> Option<CanvasRect> {
        union_bounds(self.bitmaps.iter())
    }

    /// Композиция на прозрачный холст: RGBA с прямой (не премультиплицированной) альфой,
    /// `width * height * 4` байт, строки сверху вниз. Нужна тестам и ручной приёмке
    /// (сохранение PNG); GPU-рендер её не использует.
    ///
    /// Формула оператора «over» в целых числах (все значения 0..=255, `div255(x) = (x + 127) / 255`):
    /// для слоя `s` поверх накопленного `d`
    /// `a_s = div255(coverage * color.alpha)`,
    /// `a_out = a_s + div255(a_d * (255 - a_s))`,
    /// `c_out = round((c_s * a_s * 255 + c_d * a_d * (255 - a_s)) / (a_out * 255))`;
    /// при `a_out = 0` пиксель остаётся `0,0,0,0`.
    pub fn composite_rgba_over_transparent(&self) -> Vec<u8> {
        let canvas_width = self.canvas.width() as usize;
        let canvas_height = self.canvas.height() as usize;
        let mut pixels = vec![0_u8; canvas_width.saturating_mul(canvas_height).saturating_mul(4)];
        for bitmap in &self.bitmaps {
            composite_bitmap(&mut pixels, canvas_width, bitmap);
        }
        pixels
    }
}

/// Картинка целиком внутри холста (арифметика в i64 — без переполнений).
fn fits_inside(canvas: CanvasSize, bitmap: &SubtitleBitmap) -> bool {
    let origin = bitmap.origin();
    let right = i64::from(origin.x) + i64::from(bitmap.width());
    let bottom = i64::from(origin.y) + i64::from(bitmap.height());
    origin.x >= 0
        && origin.y >= 0
        && right <= i64::from(canvas.width())
        && bottom <= i64::from(canvas.height())
}

/// Деление на 255 с округлением к ближайшему.
fn div255(value: u32) -> u32 {
    (value + 127) / 255
}

/// Накладывает одну картинку на холст `pixels` (RGBA, ширина `canvas_width`).
fn composite_bitmap(pixels: &mut [u8], canvas_width: usize, bitmap: &SubtitleBitmap) {
    if bitmap.pixel_format() == BitmapPixelFormat::PremultipliedRgba {
        picture::composite_picture(pixels, canvas_width, bitmap);
        return;
    }
    let canvas_height = pixels.len() / 4 / canvas_width.max(1);
    // Инвариант «внутри холста» проверен в `SubtitleOverlay::new`, но на случай прямого
    // вызова картинка обрезается по холсту по обеим осям (как RGBA-путь), без паники и
    // без «заворота» правой части на следующую строку.
    let (Some(columns), Some(rows)) = (
        picture::clip_axis(bitmap.origin().x, bitmap.width(), canvas_width),
        picture::clip_axis(bitmap.origin().y, bitmap.height(), canvas_height),
    ) else {
        return;
    };
    let color = bitmap.color();
    for row_offset in 0..rows.visible {
        let Some(coverage_row) = bitmap.coverage_row((rows.skipped + row_offset) as u32) else {
            return;
        };
        let row_start = (rows.destination + row_offset) * canvas_width + columns.destination;
        let visible = coverage_row
            .iter()
            .skip(columns.skipped)
            .take(columns.visible);
        for (column, &coverage) in visible.enumerate() {
            let offset = (row_start + column) * 4;
            let Some(pixel) = pixels.get_mut(offset..offset + 4) else {
                return;
            };
            blend_pixel(pixel, color, coverage);
        }
    }
}

/// Оператор over одного пикселя (формула — в доке `composite_rgba_over_transparent`).
fn blend_pixel(pixel: &mut [u8], color: SubtitleColor, coverage: u8) {
    let source_alpha = div255(u32::from(coverage) * u32::from(color.alpha));
    if source_alpha == 0 {
        return;
    }
    let [dest_red, dest_green, dest_blue, dest_alpha] = match *pixel {
        [red, green, blue, alpha] => [red, green, blue, alpha].map(u32::from),
        _ => return,
    };
    let kept = 255 - source_alpha;
    let out_alpha = source_alpha + div255(dest_alpha * kept);
    if out_alpha == 0 {
        return;
    }
    let denominator = out_alpha * 255;
    let mix = |source_channel: u8, dest_channel: u32| -> u8 {
        let numerator =
            u32::from(source_channel) * source_alpha * 255 + dest_channel * dest_alpha * kept;
        // Результат не превышает 255 по построению; `min` страхует от ошибок округления.
        u8::try_from(((numerator + denominator / 2) / denominator).min(255)).unwrap_or(u8::MAX)
    };
    let out = [
        mix(color.red, dest_red),
        mix(color.green, dest_green),
        mix(color.blue, dest_blue),
        u8::try_from(out_alpha.min(255)).unwrap_or(u8::MAX),
    ];
    pixel.copy_from_slice(&out);
}

/// Определяет изменение относительно предыдущего результата
/// (libass 0.17.5: `ass_render.c:ass_detect_change` + `ass_image_compare`).
///
/// - длины списков разные -> [`OverlayChange::Replaced`];
/// - для пары картинок: размер, шаг строки, цвет, вид или другое окно маски (другой `Arc`
///   либо смещение, [`SubtitleBitmap::shows_same_pixels_as`]) -> `Replaced`; только позиция -> `Moved`;
/// - иначе [`OverlayChange::Unchanged`].
///
/// Как и в libass, сравнение маски идёт по указателю, не по содержимому: растеризатор
/// обязан отдавать из кэша тот же `Arc`, если картинка не менялась.
pub(crate) fn detect_overlay_change(
    previous: &[SubtitleBitmap],
    current: &[SubtitleBitmap],
) -> OverlayChange {
    if previous.len() != current.len() {
        return OverlayChange::Replaced;
    }
    let mut change = OverlayChange::Unchanged;
    for (old, new) in previous.iter().zip(current) {
        let same_content = old.shows_same_pixels_as(new)
            && old.color == new.color
            && old.kind == new.kind
            && old.placement_group == new.placement_group;
        if !same_content {
            return OverlayChange::Replaced;
        }
        if old.origin != new.origin {
            change = OverlayChange::Moved;
        }
    }
    change
}

#[cfg(test)]
mod tests;
