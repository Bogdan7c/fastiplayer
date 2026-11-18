// Общие куски страниц ленты коротких видео (сессия 16b2, сценарий `capture --page feed`): вертикальная
// лента из трёх роликов в рост окна, которую листают колесом мыши, как Instagram, YouTube Shorts и TikTok.
//
// Три ролика ленты — ассеты `reel-1..3` каталога медиа: разной длины, разного размера кадра и с выжженной
// подписью `REEL N`. По размеру кадра стенд узнаёт в журнале плеера (`Browser quality shown …`), какой
// именно ролик сейчас показывает окно.
//
// Журнал страницы (POST /bench/log, префикс `feed:` или `feed-hover:`), по нему судит Rust:
//   `handoff-ready`                          — первый ролик играет, робот может отдавать вкладку;
//   `wheel trusted=<bool> dy=<число> y=<scrollY>` — настоящее колесо дошло до страницы;
//   `reel-started n=<номер> shape=<форма> muted=<bool>` — ролик ИГРАЕТ (часы ушли вперёд);
//   `reel-switch from=<n> to=<n> shape=<форма>`   — страница начала переход на следующий ролик;
//   `ack <команда>`                          — команда робота выполнена.

import { BenchPlayer, standardRefs } from './bench-player.js';
import { waitFor } from './bench-checks.js';
import { CommandChannel } from './handoff-mode.js';

/** Сколько роликов в ленте и как они называются в каталоге. */
export const REEL_COUNT = 3;
export const reelAssetId = (number) => `reel-${number}`;

/** Ролик «заиграл»: часы ушли вперёд хотя бы на столько, с. */
const PLAYING_ADVANCE_S = 0.4;
/** Сколько ждём, что ролик заиграл, мс. */
const PLAY_WAIT_MS = 15000;
/** Какая доля секции ролика должна быть видна, чтобы лента считала его «текущим». */
const CURRENT_VISIBLE_RATIO = 0.6;
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Счётчики страницы для итогового отчёта (метрики). */
export const feedState = { reelsStarted: 0 };

/** Источник роликов ленты: каталог медиа и общий журнал страницы. */
export class ReelSource {
  constructor(context, catalog) {
    this.context = context;
    this.catalog = catalog;
  }

  /** Дорожки ролика `number` (видео + звук). */
  refsOf(number) {
    return standardRefs(this.catalog, reelAssetId(number));
  }

  /**
   * Новый `<video>` с загруженным (но НЕ запущенным) роликом `number`: так сайты готовят следующий ролик.
   * Возвращает { video, player }.
   */
  async prepare(number, { muted }) {
    const video = document.createElement('video');
    video.className = 'reel-video';
    video.playsInline = true;
    video.muted = muted;
    video.dataset.reel = String(number);
    const player = new BenchPlayer(video, this.catalog, { log: (line) => this.context.log(line) });
    await player.start({ refs: this.refsOf(number), autoplay: false });
    return { video, player };
  }
}

/**
 * Вертикальная лента: секции в рост окна (`100vh`), прилипание к границе секции (scroll-snap). Следит за
 * тем, какая секция видна, и вызывает `onAdvance(from, to)`, когда лента ушла на следующий ролик.
 *
 * Прокручивается либо сам документ (раскладка `document`), либо блок ленты внутри неподвижного документа
 * (раскладка `inner-scroller`, так устроен живой Instagram: `overflow-y:auto` у блока, документ не листается).
 */
export class ReelFeed {
  /**
   * @param {object} options
   * @param {boolean} options.waitUntilOldIsGone переход только когда прежняя секция ушла с экрана ПОЛНОСТЬЮ;
   *   иначе — как только следующая видна достаточно (Shorts, живой Instagram).
   * @param {Element|null} [options.scroller] блок, который прокручивается вместо документа (`null` — документ).
   */
  constructor(context, { waitUntilOldIsGone, scroller = null }) {
    this.context = context;
    this.waitUntilOldIsGone = waitUntilOldIsGone;
    this.scroller = scroller;
    this.sections = [];
    this.ratios = new Array(REEL_COUNT + 1).fill(0);
    this.current = 1;
    this.advancing = false;
    this.lastBlocked = null;
    this.onAdvance = async () => {};
  }

  /** Строит секции `reel-1..N` внутри `parent` (по одной на ролик, одинаковой высоты). */
  mount(parent) {
    for (let number = 1; number <= REEL_COUNT; number += 1) {
      const section = document.createElement('section');
      section.className = 'reel';
      section.id = `reel-section-${number}`;
      section.dataset.reel = String(number);
      const caption = document.createElement('div');
      caption.className = 'reel-caption';
      caption.textContent = `REEL ${number}`;
      section.append(caption);
      parent.append(section);
      this.sections[number] = section;
    }
  }

  /** Секция ролика `number`. */
  sectionOf(number) {
    return this.sections[number];
  }

  /**
   * Начинает следить за секциями по событиям прокрутки. Видимую долю секции считаем по её прямоугольнику
   * при каждой прокрутке (не `IntersectionObserver`): наблюдатель считает секцию, лишь касающуюся края окна,
   * «пересекающейся» и не сообщает о точном нуле, а странице нужен момент, когда ролик ушёл ПОЛНОСТЬЮ.
   */
  watch(onAdvance) {
    this.onAdvance = onAdvance;
    let scheduled = false;
    (this.scroller ?? window).addEventListener('scroll', () => {
      if (scheduled) return;
      scheduled = true;
      requestAnimationFrame(() => { scheduled = false; this.#measure(); this.#maybeAdvance(); });
    }, { passive: true });
    this.#measure();
  }

  /** Верх и низ видимой области ленты в координатах окна: окно документа или прямоугольник блока ленты. */
  #visibleBand() {
    if (this.scroller === null) return { top: 0, bottom: window.innerHeight };
    const { top, bottom } = this.scroller.getBoundingClientRect();
    return { top, bottom };
  }

  /** Обновляет видимую долю каждой секции (0..1) по её прямоугольнику в видимой области ленты. */
  #measure() {
    const band = this.#visibleBand();
    for (let number = 1; number <= REEL_COUNT; number += 1) {
      const { top, bottom, height } = this.sections[number].getBoundingClientRect();
      const visiblePx = Math.max(0, Math.min(bottom, band.bottom) - Math.max(top, band.top));
      this.ratios[number] = height > 0 ? visiblePx / height : 0;
    }
  }

  /** Страница сама листает ленту до ролика `number` (обычный режим без робота: колеса нет). */
  scrollToReel(number) {
    if (this.scroller === null) {
      window.scrollTo({ top: (number - 1) * window.innerHeight, behavior: 'instant' });
      return;
    }
    // Блок ленты прилипает к середине секции: ставим её по центру, как это сделала бы прокрутка.
    this.sections[number].scrollIntoView({ block: 'center', behavior: 'instant' });
  }

  #maybeAdvance() {
    const next = this.current + 1;
    if (next > REEL_COUNT) return;
    const blocked = this.advancing ? 'advancing'
      : (this.ratios[next] < CURRENT_VISIBLE_RATIO ? 'next-not-visible'
        : (this.waitUntilOldIsGone && this.ratios[this.current] > 0 ? 'old-still-visible' : null));
    if (blocked) {
      // Причина отказа — в журнал один раз на смену причины (отладка прокрутки на стенде).
      if (blocked !== this.lastBlocked && this.ratios[next] > 0) {
        this.context.log(`advance-blocked next=${next} why=${blocked} ratios=${this.ratios.slice(1).map((r) => r.toFixed(2)).join(',')}`);
      }
      this.lastBlocked = blocked;
      return;
    }
    this.lastBlocked = null;
    this.advancing = true;
    const from = this.current;
    this.current = next;
    this.onAdvance(from, next)
      .catch((error) => this.context.check('feed-advance-completed', false, `${error?.name}: ${error?.message}`))
      .finally(() => { this.advancing = false; this.#maybeAdvance(); });
  }
}

/** Ждёт, пока `video` реально играет (не на паузе, часы пошли); true, если дождались. */
export async function waitUntilPlaying(video) {
  const start = video.currentTime;
  return waitFor(() => !video.paused && video.currentTime >= start + PLAYING_ADVANCE_S, PLAY_WAIT_MS);
}

/** Пишет в журнал `reel-started` для ролика `number`, когда он заиграл; false, если не заиграл. */
export async function announceReelStarted(context, video, number, shape) {
  const playing = await waitUntilPlaying(video);
  if (playing) {
    context.log(`reel-started n=${number} shape=${shape} muted=${video.muted} t=${context.elapsedMs()}`);
    feedState.reelsStarted += 1;
  }
  return playing;
}

/**
 * Настоящее колесо: каждое событие `wheel` в журнал (стенд видит, дошло ли колесо до страницы).
 * Возвращает счётчики { total, trusted }: событие из кода страницы `isTrusted` не имеет.
 */
export function logWheelEvents(context) {
  const stats = { total: 0, trusted: 0 };
  window.addEventListener('wheel', (event) => {
    stats.total += 1;
    if (event.isTrusted) stats.trusted += 1;
    context.log(`wheel trusted=${event.isTrusted} dy=${event.deltaY} y=${Math.round(window.scrollY)}`);
  }, { passive: true, capture: true });
  return stats;
}

/**
 * Служит командам робота до `finish`: `handed-off` и прочие подтверждает, на `finish` вызывает `onFinish`.
 * Хуки страницы (`hook:<имя>`) исполняет `hooks[имя]`.
 */
export async function serveRobotCommands(context, { hooks = {}, onFinish }) {
  const commands = new CommandChannel(context);
  for (;;) {
    const command = await commands.next();
    if (command === 'finish') {
      await onFinish();
      context.log('ack finish');
      return;
    }
    if (command.startsWith('hook:')) {
      const hook = hooks[command.slice('hook:'.length)];
      if (!hook) context.check(`known-hook-${command}`, false, `no hook '${command}' on this page`);
      else await hook();
    }
    context.log(`ack ${command}`);
  }
}

/** Случайная пауза в диапазоне, мс (сайты запускают следующий ролик не мгновенно). */
export function randomDelayMs(minMs, maxMs) {
  return Math.round(minMs + Math.random() * (maxMs - minMs));
}

export { sleep };
