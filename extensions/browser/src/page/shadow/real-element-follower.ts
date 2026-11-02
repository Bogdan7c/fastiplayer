// Подтягивание настоящего элемента к позиции тени (находка 00c).
//
// Браузер чистит буфер MSE относительно позиции НАСТОЯЩЕГО элемента. Если он стоит на месте
// передачи, а тень ушла далеко вперёд, плеер страницы упирается в `QuotaExceededError`
// (YouTube в фоне — на 7-й минуте). Лечение из 00c: изредка ставить настоящему элементу
// (на паузе) позицию тени. Цена — Chromium декодирует 4–8 кадров на перестановку, поэтому
// перестановок не больше, чем нужно.
//
// Проверка идёт на тиках часов плеера, а не своим таймером: таймеры скрытой вкладки Chromium
// будит раз в минуту, а тики приходят через порт экстеншена без троттлинга (00c), и без игры
// плеера (нет тиков) двигать нечего.

/** Как часто сверять позиции: 15 с из 00c (13 минут YouTube в фоне без остановок). */
export const FOLLOW_CHECK_INTERVAL_MS = 15_000;

/**
 * Насколько настоящий элемент может разойтись с тенью, прежде чем его переставят: 20 с из 00c.
 * Сравнивается модуль разницы: после перемотки тени назад настоящий элемент оказывается
 * впереди, и браузер выбрасывал бы данные вокруг позиции тени как «прошлое».
 */
export const FOLLOW_MAX_DRIFT_US = 20_000_000;

/** Что подтягиванию нужно от контроллера тени. */
export interface FollowerPort {
  /** Можно ли сейчас переставлять: тень не в перемотке, у настоящего элемента есть метаданные. */
  canReposition(): boolean;
  shadowPositionUs(): number;
  realPositionUs(): number;
  /** Поставить настоящему элементу позицию (оригинальным сеттером; он на паузе). */
  repositionReal(positionUs: number): void;
}

export class RealElementFollower {
  readonly #port: FollowerPort;
  #lastCheckAtMs: number;

  constructor(port: FollowerPort, startedAtMs: number) {
    this.#port = port;
    this.#lastCheckAtMs = startedAtMs;
  }

  /** Тик часов: раз в `FOLLOW_CHECK_INTERVAL_MS` сверить и, если разошлись, переставить. */
  onClockTick(nowMs: number): void {
    if (nowMs - this.#lastCheckAtMs < FOLLOW_CHECK_INTERVAL_MS) return;
    this.#lastCheckAtMs = nowMs;
    if (!this.#port.canReposition()) return;
    const shadowUs = this.#port.shadowPositionUs();
    if (Math.abs(shadowUs - this.#port.realPositionUs()) <= FOLLOW_MAX_DRIFT_US) return;
    this.#port.repositionReal(shadowUs);
  }
}
