// Ведро жетонов: «не больше N единиц за скользящую секунду». Единственная реализация окна лимитов
// страницы: и событий в изолированном мире (content/page-event-budget.ts), и байтов в MAIN-мире и
// воркере (page/forward-byte-meter.ts). Единицы — что угодно (события, байты), логика одна.
//
// Жетоны капают непрерывно со скоростью `perSecond`, запас не больше `perSecond`. Поэтому за любую
// секунду проходит не больше двух `perSecond` (полный запас плюс накапанное), а за длинный отрезок —
// ровно `perSecond` в секунду. Всплеск до полного запаса разрешён: живая страница шлёт пачками.

/**
 * Часы в миллисекундах. Ждём монотонные, но ведро обязано пережить и другие: шаг назад переносит точку
 * отсчёта на новое время (иначе ведро замёрзло бы, пока часы не догонят старую отметку).
 */
export type MillisecondClock = () => number;

export interface TokenBucket {
  /**
   * Взять `amount` единиц. `false` — столько сейчас нет или `amount` не конечное неотрицательное
   * число; запас в обоих случаях не тронут.
   */
  take(amount: number): boolean;
}

/** Миллисекунд в секунде окна: скорость задаётся «в секунду», часы идут в миллисекундах. */
const MILLISECONDS_PER_SECOND = 1_000;

export function createTokenBucket(perSecond: number, now: MillisecondClock): TokenBucket {
  let tokens = perSecond;
  let lastRefillMs = now();

  function refill(): void {
    const currentMs = now();
    // Сломанные часы (NaN) запас не трогают: один NaN в `tokens` навсегда открыл бы ведро.
    if (!Number.isFinite(currentMs)) return;
    const elapsedMs = currentMs - lastRefillMs;
    lastRefillMs = currentMs;
    if (elapsedMs > 0) tokens = Math.min(perSecond, tokens + (elapsedMs * perSecond) / MILLISECONDS_PER_SECOND);
  }

  return {
    take(amount) {
      refill();
      // Длину присылает вызывающий: NaN прошёл бы сравнение ниже и отравил запас навсегда, а
      // отрицательное число пополнило бы его.
      if (!Number.isFinite(amount) || amount < 0 || amount > tokens) return false;
      tokens -= amount;
      return true;
    },
  };
}
