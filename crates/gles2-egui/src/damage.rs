//! Частичный показ: чистая логика «что перерисовать в этом кадре» без GL и EGL.
//!
//! # Зачем
//! На слабом GPU (Intel Ironlake) полный проход видео на 1440×900 стоит ~5 мс, а кадры, где
//! поменялся только интерфейс (наведение мыши, подсказка, пауза), видео не трогают. Если драйвер
//! сообщает возраст заднего буфера (`EGL_EXT_buffer_age`), то перерисовывать нужно только то, что
//! менялось за последние `age` кадров, и сообщить композитору, что изменилось в этом кадре
//! (`EGL_KHR_swap_buffers_with_damage` / `EGL_EXT_swap_buffers_with_damage`).
//!
//! # Контракт возраста (`EGL_EXT_buffer_age`)
//! `age = N > 0` — в заднем буфере лежит содержимое кадра, показанного N кадров назад; `0` —
//! содержимое не определено. Значит, чтобы буфер стал равен текущему кадру, надо перерисовать
//! объединение повреждений текущего кадра и `N − 1` предыдущих.
//!
//! # Модель
//! [`DamageHistory`] хранит повреждения последних показанных кадров. [`DamageHistory::plan`]
//! решает: полная перерисовка ([`FullRepaintReason`] — причина типизирована, считается в
//! счётчиках) или частичная ([`TargetArea::Region`] — ограничивающий прямоугольник объединения).
//! Прямоугольник один, а не список: ограничивающий прямоугольник чуть шире точного объединения,
//! но это только лишние пиксели, не ошибка, а один scissor на слой дешевле списка.
//!
//! Все координаты — физические пиксели цели с началом в ЛЕВОМ ВЕРХНЕМ углу. В «низ-лево» (так
//! требуют scissor GL и прямоугольники EGL) они переводятся на границе с драйвером.

use std::collections::VecDeque;

use std::fmt;

use crate::capabilities::Gles2Capabilities;
use crate::context::driver::SurfaceSize;

#[cfg(test)]
mod tests;

/// Сколько прошлых кадров помнит история. Задний буфер старше `MAX + 1` кадров (кольцо из
/// 6+ буферов у Mesa не бывает; обычно 2–4) перерисовывается целиком.
pub const DAMAGE_HISTORY_FRAMES: usize = 4;

/// Прямоугольник цели в физических пикселях, начало — левый верхний угол.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelRect {
    /// Левая граница.
    pub x: u32,
    /// Верхняя граница.
    pub y: u32,
    /// Ширина.
    pub width: u32,
    /// Высота.
    pub height: u32,
}

impl PixelRect {
    /// Прямоугольник на всю цель.
    #[must_use]
    pub const fn whole(size: SurfaceSize) -> Self {
        Self {
            x: 0,
            y: 0,
            width: size.width(),
            height: size.height(),
        }
    }

    /// Пустой ли (нулевая площадь).
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Площадь в пикселях.
    #[must_use]
    pub const fn area(self) -> u64 {
        self.width as u64 * self.height as u64
    }

    const fn right(self) -> u64 {
        self.x as u64 + self.width as u64
    }

    const fn bottom(self) -> u64 {
        self.y as u64 + self.height as u64
    }

    /// Общая часть; `None`, если прямоугольники не пересекаются.
    #[must_use]
    pub fn intersection(self, other: Self) -> Option<Self> {
        let left = self.x.max(other.x);
        let top = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        let width = u32::try_from(right.checked_sub(u64::from(left))?).ok()?;
        let height = u32::try_from(bottom.checked_sub(u64::from(top))?).ok()?;
        let rect = Self {
            x: left,
            y: top,
            width,
            height,
        };
        (!rect.is_empty()).then_some(rect)
    }

    /// Ограничивающий прямоугольник двух прямоугольников. Пустой операнд не расширяет результат.
    #[must_use]
    pub fn bounding_union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let left = self.x.min(other.x);
        let top = self.y.min(other.y);
        // Правая и нижняя границы не превышают `u32::MAX` у входов, которые помещаются в цель.
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Self {
            x: left,
            y: top,
            width: u32::try_from(right - u64::from(left)).unwrap_or(u32::MAX),
            height: u32::try_from(bottom - u64::from(top)).unwrap_or(u32::MAX),
        }
    }
}

/// Объединение двух необязательных повреждений (`None` — ничего не менялось).
#[must_use]
pub fn union_damage(first: Option<PixelRect>, second: Option<PixelRect>) -> Option<PixelRect> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.bounding_union(second)),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

/// Область цели: что перерисовывать и что сообщать композитору как изменённое.
///
/// Инвариант для слоёв: пока кадр ограничен областью, слой не имеет права менять пиксели вне её
/// (см. `ActiveFrame::repaint_area`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetArea {
    /// Вся цель.
    Whole,
    /// Только этот непустой прямоугольник.
    Region(PixelRect),
    /// Ничего: рисовать нечего.
    Nothing,
}

impl TargetArea {
    /// Область из необязательного повреждения (`None` — ничего).
    #[must_use]
    pub fn from_damage(damage: Option<PixelRect>) -> Self {
        damage.map_or(Self::Nothing, Self::Region)
    }

    /// Площадь в пикселях при цели `target`.
    #[must_use]
    pub fn pixels(self, target: SurfaceSize) -> u64 {
        match self {
            Self::Whole => PixelRect::whole(target).area(),
            Self::Region(rect) => rect.area(),
            Self::Nothing => 0,
        }
    }

    /// Часть `rect`, которую слою разрешено рисовать; `None` — слою здесь делать нечего.
    #[must_use]
    pub fn clip(self, rect: PixelRect) -> Option<PixelRect> {
        match self {
            Self::Whole => (!rect.is_empty()).then_some(rect),
            Self::Region(allowed) => rect.intersection(allowed),
            Self::Nothing => None,
        }
    }
}

/// Возраст заднего буфера (`EGL_EXT_buffer_age`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BufferAge {
    /// Драйвер возраст не сообщает (нет расширения, окно отпущено, offscreen без кольца).
    Unsupported,
    /// `0`: содержимое буфера не определено (новый буфер, после resize).
    Undefined,
    /// `N ≥ 1` кадров назад.
    Frames(u32),
}

impl BufferAge {
    /// Из значения `EGL_BUFFER_AGE_EXT`: `0` — не определено.
    #[must_use]
    pub const fn from_egl_value(value: u32) -> Self {
        if value == 0 {
            Self::Undefined
        } else {
            Self::Frames(value)
        }
    }
}

/// Изменились ли слои под интерфейсом (видео, субтитры) относительно прошлого кадра.
/// Сообщает вызывающий: только он знает, поменялся ли кадр видео, вьюпорт, исключения, оверлей.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayersDamage {
    /// Слои рисуют те же пиксели, что в прошлом кадре: можно перерисовать только часть.
    Unchanged,
    /// Слои изменились (или неизвестно): перерисовывается вся цель.
    Changed,
}

/// Почему кадр перерисован целиком.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FullRepaintReason {
    /// Драйвер не сообщает возраст буфера.
    BufferAgeUnsupported,
    /// Возраст 0: содержимое буфера не определено.
    BufferAgeUndefined,
    /// Размер цели изменился: история прошлого размера бессмысленна.
    TargetResized,
    /// Буфер старше, чем помнит история.
    BufferTooOld,
    /// Слои (видео, субтитры) изменились.
    LayersChanged,
    /// Прошлый кадр бросили на полпути: буфер содержит недорисованное.
    PreviousFrameAbandoned,
    /// Область перерисовки покрывает всю цель: частичный путь ничего не экономит.
    DamageCoversTarget,
}

/// Решение о перерисовке кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepaintPlan {
    /// Перерисовать всю цель и показать целиком.
    Full(FullRepaintReason),
    /// Перерисовать `repaint`, а композитору сообщить `changed`.
    Partial {
        /// Что рисовать (объединение повреждений за `age` кадров): `Region` или `Nothing`.
        repaint: TargetArea,
        /// Что изменилось в ЭТОМ кадре относительно прошлого показанного (для damage-swap).
        changed: TargetArea,
    },
}

impl RepaintPlan {
    /// Что кадр с этим планом изменил относительно прошлого ПОКАЗАННОГО кадра — запись для
    /// истории. Это не то же, что перерисовано: полный кадр из-за устаревшего буфера (возраст 0,
    /// слишком старый, неизвестный) меняет на экране только то, что изменил интерфейс, и
    /// записывать его «всей целью» нельзя — следующие кадры при глубокой цепочке показа тогда
    /// всегда были бы полными (каскад). Всей целью записываются кадры, где менялось и то, чего
    /// не видит отпечаток интерфейса: слои, размер цели, брошенный прошлый кадр.
    #[must_use]
    pub fn presented_change(
        self,
        layers: LayersDamage,
        ui_damage: Option<PixelRect>,
    ) -> TargetArea {
        match self {
            Self::Partial { changed, .. } => changed,
            Self::Full(reason) => {
                let changes_beyond_ui = layers == LayersDamage::Changed
                    || matches!(
                        reason,
                        FullRepaintReason::LayersChanged
                            | FullRepaintReason::TargetResized
                            | FullRepaintReason::PreviousFrameAbandoned
                    );
                if changes_beyond_ui {
                    TargetArea::Whole
                } else {
                    TargetArea::from_damage(ui_damage)
                }
            }
        }
    }
}

/// Повреждения последних показанных кадров. Владеет логикой возраста; больше никто не
/// рассуждает о том, какие кадры лежат в каком буфере.
#[derive(Debug, Clone, Default)]
pub struct DamageHistory {
    /// Размер цели записанных кадров; `None` — ничего не показано.
    target: Option<SurfaceSize>,
    /// Новейший кадр первым. `None` в записи — кадр ничего не менял.
    frames: VecDeque<Option<PixelRect>>,
    /// Прошлый кадр бросили (ошибка рисования): ближайший кадр обязан быть полным.
    abandoned: bool,
}

impl DamageHistory {
    /// Пустая история.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Решает, как перерисовать кадр.
    ///
    /// - `age` — возраст заднего буфера, запрошенный ДО рисования;
    /// - `target` — размер цели;
    /// - `layers` — изменились ли слои под интерфейсом;
    /// - `ui_damage` — что изменил интерфейс в этом кадре (`None` — ничего).
    ///
    /// Порядок проверок задаёт причину полной перерисовки: от самых «глубоких» (драйвер вообще
    /// не умеет) к частным.
    #[must_use]
    pub fn plan(
        &self,
        age: BufferAge,
        target: SurfaceSize,
        layers: LayersDamage,
        ui_damage: Option<PixelRect>,
    ) -> RepaintPlan {
        let frames_back = match age {
            BufferAge::Unsupported => {
                return RepaintPlan::Full(FullRepaintReason::BufferAgeUnsupported);
            }
            BufferAge::Undefined => {
                return RepaintPlan::Full(FullRepaintReason::BufferAgeUndefined);
            }
            BufferAge::Frames(frames_back) => frames_back,
        };
        if self.abandoned {
            return RepaintPlan::Full(FullRepaintReason::PreviousFrameAbandoned);
        }
        if self.target != Some(target) {
            return RepaintPlan::Full(FullRepaintReason::TargetResized);
        }
        // Буфер возраста N равен кадру N-го назад: нужны повреждения N−1 прошлых кадров.
        let previous_needed = usize::try_from(frames_back - 1).unwrap_or(usize::MAX);
        if previous_needed > self.frames.len() {
            return RepaintPlan::Full(FullRepaintReason::BufferTooOld);
        }
        if layers == LayersDamage::Changed {
            return RepaintPlan::Full(FullRepaintReason::LayersChanged);
        }
        let repaint = self
            .frames
            .iter()
            .take(previous_needed)
            .fold(ui_damage, |bounds, damage| union_damage(bounds, *damage));
        if repaint.is_some_and(|rect| rect == PixelRect::whole(target)) {
            return RepaintPlan::Full(FullRepaintReason::DamageCoversTarget);
        }
        RepaintPlan::Partial {
            repaint: TargetArea::from_damage(repaint),
            changed: TargetArea::from_damage(ui_damage),
        }
    }

    /// Записать показанный кадр. `damage` — что кадр изменил относительно прошлого показанного
    /// (полная перерисовка — вся цель).
    pub fn record_presented(&mut self, target: SurfaceSize, damage: TargetArea) {
        if self.target != Some(target) {
            // Другой размер: прошлые записи относились к другим буферам.
            self.frames.clear();
            self.target = Some(target);
        }
        self.abandoned = false;
        let entry = match damage {
            TargetArea::Whole => Some(PixelRect::whole(target)),
            TargetArea::Region(rect) => Some(rect),
            TargetArea::Nothing => None,
        };
        self.frames.push_front(entry);
        self.frames.truncate(DAMAGE_HISTORY_FRAMES);
    }

    /// Кадр начат, но не показан (ошибка рисования): задний буфер может содержать полкадра, а
    /// возраст драйвера этого не знает. Следующий кадр — полный.
    pub fn record_abandoned(&mut self) {
        self.abandoned = true;
    }

    /// Забыть всё (окно отпущено и возвращено, контекст пересоздан).
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

impl FullRepaintReason {
    /// Все причины (порядок задаёт индексы счётчиков).
    pub const ALL: [Self; 7] = [
        Self::BufferAgeUnsupported,
        Self::BufferAgeUndefined,
        Self::TargetResized,
        Self::BufferTooOld,
        Self::LayersChanged,
        Self::PreviousFrameAbandoned,
        Self::DamageCoversTarget,
    ];

    const fn index(self) -> usize {
        match self {
            Self::BufferAgeUnsupported => 0,
            Self::BufferAgeUndefined => 1,
            Self::TargetResized => 2,
            Self::BufferTooOld => 3,
            Self::LayersChanged => 4,
            Self::PreviousFrameAbandoned => 5,
            Self::DamageCoversTarget => 6,
        }
    }
}

/// Счётчики частичного показа за всё время: по ним доказывается, что трюк работает (кадры с
/// одним интерфейсом перерисовывают малую часть цели), а не «функция вызвалась».
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PartialPresentStats {
    /// Показанных кадров с частичной перерисовкой.
    pub partial_presents: u64,
    /// Из них показано через damage-swap (композитору сообщён прямоугольник).
    pub damage_swaps: u64,
    /// Показанных кадров с полной перерисовкой.
    pub full_presents: u64,
    /// Площадь, перерисованная всеми показанными кадрами, в пикселях.
    pub repainted_pixels: u64,
    /// Площадь, которую перерисовали бы все показанные кадры без частичного показа.
    pub target_pixels: u64,
    full_by_reason: [u64; FullRepaintReason::ALL.len()],
}

impl PartialPresentStats {
    /// Полных перерисовок по причине.
    #[must_use]
    pub fn full_presents_for(&self, reason: FullRepaintReason) -> u64 {
        self.full_by_reason[reason.index()]
    }

    /// Записать показанный кадр.
    pub(crate) fn record_presented(
        &mut self,
        plan: RepaintPlan,
        target: SurfaceSize,
        told_compositor_damage: bool,
    ) {
        let target_pixels = PixelRect::whole(target).area();
        self.target_pixels += target_pixels;
        match plan {
            RepaintPlan::Full(reason) => {
                self.full_presents += 1;
                self.full_by_reason[reason.index()] += 1;
                self.repainted_pixels += target_pixels;
            }
            RepaintPlan::Partial { repaint, .. } => {
                self.partial_presents += 1;
                self.damage_swaps += u64::from(told_compositor_damage);
                self.repainted_pixels += repaint.pixels(target);
            }
        }
    }
}

impl fmt::Display for PartialPresentStats {
    /// Одна строка итогов для лога: сколько кадров частично, по каким причинам целиком, сколько
    /// пикселей перерисовано против полной цели.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "partial presents {} (damage swaps {}), full presents {} (",
            self.partial_presents, self.damage_swaps, self.full_presents
        )?;
        for (index, reason) in FullRepaintReason::ALL.iter().enumerate() {
            let separator = if index == 0 { "" } else { ", " };
            write!(
                formatter,
                "{separator}{reason:?}: {}",
                self.full_presents_for(*reason)
            )?;
        }
        write!(
            formatter,
            "), repainted {} of {} target pixels",
            self.repainted_pixels, self.target_pixels
        )
    }
}

/// Что драйвер даёт для частичного показа: возраст буфера и какое расширение показа с
/// повреждениями используется (KHR предпочтительнее EXT, как в сессии EGL).
#[must_use]
pub fn describe_partial_present_support(capabilities: &Gles2Capabilities) -> String {
    let extensions = capabilities.egl_display_extensions();
    let buffer_age = capabilities.tiers().conveniences.buffer_age;
    let damage_swap = if extensions.contains("EGL_KHR_swap_buffers_with_damage") {
        "EGL_KHR_swap_buffers_with_damage"
    } else if extensions.contains("EGL_EXT_swap_buffers_with_damage") {
        "EGL_EXT_swap_buffers_with_damage"
    } else {
        "none"
    };
    format!("buffer age available: {buffer_age}, damage swap: {damage_swap}")
}
