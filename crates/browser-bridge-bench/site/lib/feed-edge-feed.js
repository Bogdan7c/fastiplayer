// Лента для краёв (сессия 16b3, сценарий `capture --page feed-edges`): та же вертикальная лента роликов
// в рост окна, что `ReelFeed` из feed-kit.js, но без его серийной очереди. Настоящий сайт не ждёт, пока
// закончится прежний переход: при быстрой серии прокруток шаги приходят один за другим, пока предыдущий
// ещё не доделан (Instagram I4, TikTok T3, Shorts S3). Поэтому здесь смена ролика вызывает форму СРАЗУ,
// ничего не дожидаясь, и считает, что произошло в серии.
//
// Журнал страницы (префикс `feed-edges:`), по нему судит Rust:
//   `reel-step n=<номер> kind=<normal|burst> t=<мс>` — страница перешла на ролик (burst: шаг пришёл раньше
//                                                      BURST_GAP_MS после предыдущего, то есть предыдущий
//                                                      ролик был промежуточным);
//   `feed-tail-reached from=<номер>`                — лента ушла за последний ролик в раздел без видео.

import { feedState } from './feed-kit.js';

/** Какая доля секции должна быть видна, чтобы лента считала её «текущей». */
const CURRENT_VISIBLE_RATIO = 0.6;
/** Шаг, пришедший раньше чем через столько мс после предыдущего, — часть быстрой серии (I4). */
export const BURST_GAP_MS = 1000;

/** Хвост ленты: ничего или раздел без видео после последнего ролика (комментарии, конец ленты). */
export const TAIL_NONE = 'none';
export const TAIL_COMMENTS = 'comments';
export const TAILS = [TAIL_NONE, TAIL_COMMENTS];

/**
 * Как лента листается колесом: `native` — родная прокрутка с прилипанием (Instagram: Firefox гасит быстрые
 * щелчки, пока доезжает до границы секции); `wheel` — страница сама ловит колесо и переходит ровно на
 * следующую секцию на каждый жест (TikTok и Shorts листают так, поэтому быстрая серия там быстрая).
 */
export const PAGING_NATIVE = 'native';
export const PAGING_WHEEL = 'wheel';
export const PAGINGS = [PAGING_NATIVE, PAGING_WHEEL];
/** Жест колесом — это столько пикселей прокрутки подряд (5 щелчков стенда = 900-990 px, 4 щелчка меньше). */
const WHEEL_GESTURE_PX = 800;
/** Накопленная прокрутка колеса забывается, если щелчки пошли реже, чем раз в столько мс. */
const WHEEL_GESTURE_FORGET_MS = 300;

/** Что страница насчитала про серию шагов (уходит в отчёт метриками). */
export const stepStats = { finalReel: 1, steps: 0, burstIntermediates: 0, tailReached: 0, burstGapMinMs: null, burstGapMaxMs: null };

export class EdgeFeed {
  /**
   * @param {object} context контекст сценария (журнал)
   * @param {object} options
   * @param {number} options.reels сколько роликов в ленте
   * @param {string} options.tail `TAIL_NONE` или `TAIL_COMMENTS`
   */
  constructor(context, { reels, tail }) {
    this.context = context;
    this.reels = reels;
    this.tail = tail;
    this.sections = [];
    this.ratios = new Array(reels + 2).fill(0);
    this.current = 1;
    this.lastStepAt = null;
    /** Переход, только когда прежняя секция ушла с экрана ПОЛНОСТЬЮ (Instagram, TikTok); иначе — как только следующая видна достаточно (Shorts). */
    this.waitUntilOldIsGone = false;
    this.onStep = () => {};
    this.onTail = () => {};
  }

  /** Строит секции роликов (и раздел без видео, если он нужен) внутри `parent`. */
  mount(parent) {
    for (let number = 1; number <= this.reels; number += 1) {
      this.sections[number] = this.#section(parent, `reel-section-${number}`, `REEL ${number}`);
      this.sections[number].dataset.reel = String(number);
    }
    if (this.tail === TAIL_COMMENTS) {
      this.sections[this.reels + 1] = this.#section(parent, 'after-feed', 'COMMENTS (no video here)');
    }
  }

  #section(parent, id, caption) {
    const section = document.createElement('section');
    section.className = 'reel';
    section.id = id;
    const label = document.createElement('div');
    label.className = 'reel-caption';
    label.textContent = caption;
    section.append(label);
    parent.append(section);
    return section;
  }

  /** Секция ролика `number`. */
  sectionOf(number) {
    return this.sections[number];
  }

  /**
   * Начинает следить за прокруткой. `onStep(from, to, kind)` зовётся СИНХРОННО на каждой смене ролика
   * вперёд, `onTail(from)` — когда лента ушла в раздел без видео. Прежний переход не ждётся.
   */
  watch({ onStep, onTail }) {
    this.onStep = onStep;
    this.onTail = onTail;
    let scheduled = false;
    window.addEventListener('scroll', () => {
      if (scheduled) return;
      scheduled = true;
      requestAnimationFrame(() => { scheduled = false; this.#measure(); this.#advanceIfScrolled(); });
    }, { passive: true });
    this.#measure();
  }

  /**
   * Листание жестами колеса (`PAGING_WHEEL`): родная прокрутка отключена, каждый жест (накопленные
   * `WHEEL_GESTURE_PX` пикселей) мгновенно ставит на экран следующую секцию.
   */
  enableWheelPaging() {
    let accumulatedPx = 0;
    let lastWheelAt = 0;
    let pagedTo = 1;
    window.addEventListener('wheel', (event) => {
      event.preventDefault();
      const now = performance.now();
      if (now - lastWheelAt > WHEEL_GESTURE_FORGET_MS) accumulatedPx = 0;
      lastWheelAt = now;
      accumulatedPx += Math.max(0, event.deltaY);
      if (accumulatedPx < WHEEL_GESTURE_PX) return;
      accumulatedPx = 0;
      pagedTo = Math.min(pagedTo + 1, this.sections.length - 1);
      window.scrollTo({ top: this.sections[pagedTo].offsetTop, behavior: 'instant' });
    }, { passive: false });
  }

  /** Обновляет видимую долю каждой секции (0..1) по её прямоугольнику. */
  #measure() {
    for (let number = 1; number < this.sections.length; number += 1) {
      const { top, bottom, height } = this.sections[number].getBoundingClientRect();
      const visiblePx = Math.max(0, Math.min(bottom, window.innerHeight) - Math.max(top, 0));
      this.ratios[number] = height > 0 ? visiblePx / height : 0;
    }
  }

  /** Самая дальняя секция, видимая достаточно: к ней лента и перешла (назад не ходим). */
  #advanceIfScrolled() {
    let target = this.current;
    for (let number = this.current + 1; number < this.sections.length; number += 1) {
      if (this.ratios[number] >= CURRENT_VISIBLE_RATIO) target = number;
    }
    if (target === this.current) return;
    if (this.waitUntilOldIsGone && this.ratios[this.current] > 0) return;
    const from = this.current;
    this.current = target;
    if (target > this.reels) {
      stepStats.tailReached += 1;
      this.context.log(`feed-tail-reached from=${from}`);
      this.onTail(from);
      return;
    }
    this.#step(from, target);
  }

  #step(from, to) {
    const now = performance.now();
    const kind = this.lastStepAt !== null && now - this.lastStepAt < BURST_GAP_MS ? 'burst' : 'normal';
    // Пришёл быстрый шаг: ролик, к которому перешли перед ним, был промежуточным.
    if (kind === 'burst') {
      stepStats.burstIntermediates += 1;
      const gap = Math.round(now - this.lastStepAt);
      stepStats.burstGapMinMs = Math.min(stepStats.burstGapMinMs ?? gap, gap);
      stepStats.burstGapMaxMs = Math.max(stepStats.burstGapMaxMs ?? gap, gap);
    }
    this.lastStepAt = now;
    stepStats.steps += 1;
    stepStats.finalReel = to;
    this.context.log(`reel-step n=${to} kind=${kind} t=${this.context.elapsedMs()}`);
    this.onStep(from, to, kind);
  }

  /** Для отчёта. */
  static metrics() {
    // Промежутки между шагами серии — в отчёт: по ним видно, насколько быстро листал робот.
    const gaps = stepStats.burstGapMinMs === null ? {} : {
      burst_gap_min_ms: stepStats.burstGapMinMs,
      burst_gap_max_ms: stepStats.burstGapMaxMs,
    };
    return {
      ...gaps,
      reels_started: feedState.reelsStarted,
      final_reel: stepStats.finalReel,
      reel_steps: stepStats.steps,
      burst_intermediates: stepStats.burstIntermediates,
      tail_reached: stepStats.tailReached,
    };
  }
}
