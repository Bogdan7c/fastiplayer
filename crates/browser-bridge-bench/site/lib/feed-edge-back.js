// Прокрутка ленты НАЗАД (сессия 16b3b, страница `feed-edges`, параметр адреса `back`). Наблюдение живых
// лент (16b3, Firefox 160): назад почти всегда новый элемент с новым MediaSource, с нуля; если элемент
// Instagram ещё жив (назад сразу после вперёд), тот же элемент продолжает с секунды, где встал на паузу.
// TikTok назад — всегда новый MediaSource, с нуля.
//
// Формы возврата (классы ниже расширяют формы вперёд из feed-edge-shapes.js, шаг ВПЕРЁД у них прежний):
//  * `live` (Instagram, `shape=many`): страница ставит ролик 2 на паузу, пишет адрес ролика 1 и через
//    80-400 мс зовёт `play()` ТОГО ЖЕ элемента ролика 1: он продолжает со своей секунды;
//  * `recreated` (Instagram, `shape=many`): страница удаляет элемент ролика 1 и создаёт новый `<video>` с
//    новым MediaSource; ролик играет с нуля;
//  * `recreated` (TikTok, `shape=pool`): заготовленный элемент пула получает НОВЫЙ MediaSource ролика 1,
//    показывается и играет с нуля; адрес не меняется.
//
// Журнал страницы:
//   `reel-paused n=<номер> at_ms=<мс>` — страница остановила ролик на этой секунде (при шаге вперёд);
//   `reel-back-play-called n=<номер> shape=<форма> element=<same|new> t=<мс>` — страница ПОЗВАЛА `play()`
//     вернувшегося ролика. Это раздражитель строки: заиграет ли ролик, зависит от продукта (пока окно держит
//     часы элемента, `play()` не оживляет его), поэтому «заиграл» раздражителем быть не может;
//   `reel-back-started n=<номер> shape=<форма> element=<same|new> t=<мс>` — вернувшийся ролик заиграл
//     (часы элемента пошли). Метка отличается от `reel-started`, чтобы робот не принял старую строку за новую;
//   `play-rejected <что>: <имя ошибки>` — `play()` отклонён; судья валит строку на `AbortError`: страница не
//     должна получать ошибку, которой сама не вызывала (решение 371);
//   `stimulus reel-element-recreated n=<номер>` — страница удалила элемент ролика и создала новый.

import { randomDelayMs, waitUntilPlaying } from './feed-kit.js';
import { ManyEdge, PoolEdge, reelAddress } from './feed-edge-shapes.js';

/** Параметр адреса `back`: назад не ходим, возврат к живому элементу, возврат с пересозданием ролика. */
export const BACK_NONE = 'none';
export const BACK_LIVE = 'live';
export const BACK_RECREATED = 'recreated';
/** Возврат кнопкой «Назад» браузера после ухода из ленты (TikTok, `shape=pool`). */
export const BACK_HISTORY = 'history';
export const BACKS = [BACK_NONE, BACK_LIVE, BACK_RECREATED, BACK_HISTORY];

/** Пауза между паузой ролика 2 и `play()` вернувшегося ролика (как на обычном шаге Instagram, I1). */
const BACK_PLAY_DELAY_RANGE_MS = [80, 400];

/** Сколько раз вернувшийся ролик заиграл (уходит в отчёт метрикой `reel_back_started`). */
const backState = { started: 0 };

/** Метрики возврата для отчёта страницы. */
export function backMetrics() {
  return { reel_back_started: backState.started };
}

/** Пишет `reel-back-started`, когда вернувшийся ролик реально заиграл; false, если не заиграл. */
export async function announceBackStarted(context, video, number, shape, element) {
  const playing = await waitUntilPlaying(video);
  if (playing) {
    backState.started += 1;
    context.log(`reel-back-started n=${number} shape=${shape} element=${element} t=${context.elapsedMs()}`);
  }
  return playing;
}

/**
 * `play()` вернувшегося ролика: сначала метка «позвала» (раздражитель строки), потом сам вызов. Отказ пишется
 * в журнал (`play-rejected`) и пробрасывается вызывающему: судья валит строку на `AbortError`.
 */
export async function playForBack(context, video, number, shape, element) {
  context.log(`reel-back-play-called n=${number} shape=${shape} element=${element} t=${context.elapsedMs()}`);
  try {
    await video.play();
  } catch (error) {
    context.log(`play-rejected back reel ${number}: ${error?.name}`);
    throw error;
  }
}

/** Фоновый `playForBack`: отказ уже записан в журнал, судья его увидит, поэтому здесь он не пробрасывается. */
function playForBackLogged(context, video, number, shape, element) {
  playForBack(context, video, number, shape, element).catch(() => {});
}

/** Пишет секунду, на которой страница остановила ролик `number` (шаг вперёд, элемент уже на паузе). */
function logPausedSecond(context, video, number) {
  context.log(`reel-paused n=${number} at_ms=${Math.round(video.currentTime * 1000)}`);
}

/** Instagram: возврат к ЖИВОМУ элементу ролика 1. */
class LiveBackMany extends ManyEdge {
  step(from, to, kind) {
    super.step(from, to, kind);
    // Обычный шаг ставит прежний ролик на паузу синхронно: секунда паузы уже не меняется.
    if (kind === 'normal') logPausedSecond(this.context, this.entries.get(from).video, from);
  }

  stepBack(from, to) {
    this.currentReel = to;
    this.entries.get(from).video.pause();
    history.pushState({ reel: to }, '', reelAddress.many(to));
    setTimeout(() => {
      // Лента могла уйти дальше, пока шла пауза: запаздывающий `play()` не должен оживлять оставленный ролик.
      if (this.currentReel !== to) return;
      const video = this.entries.get(to).video;
      playForBackLogged(this.context, video, to, this.name, 'same');
      announceBackStarted(this.context, video, to, this.name, 'same');
    }, randomDelayMs(...BACK_PLAY_DELAY_RANGE_MS));
  }
}

/** Instagram: страница пересоздаёт ролик 1 — новый элемент, новый MediaSource. */
class RecreatedBackMany extends ManyEdge {
  step(from, to, kind) {
    super.step(from, to, kind);
    if (kind === 'normal') logPausedSecond(this.context, this.entries.get(from).video, from);
  }

  stepBack(from, to) {
    this.currentReel = to;
    this.entries.get(from).video.pause();
    history.pushState({ reel: to }, '', reelAddress.many(to));
    // Старый элемент ролика уходит из документа, на его месте строится новый.
    const stale = this.entries.get(to);
    stale.video.remove();
    stale.video.pause();
    this.context.log(`stimulus reel-element-recreated n=${to}`);
    this.#recreate(to).catch((error) => this.context.check('back-recreate-completed', false, `${error?.name}: ${error?.message}`));
  }

  async #recreate(number) {
    const entry = await this.source.prepare(number, { muted: true });
    // Лента могла уйти дальше, пока строился элемент: оставленный ролик не оживляем.
    if (this.currentReel !== number) return;
    this.entries.set(number, entry);
    this.feed.sectionOf(number).append(entry.video);
    playForBackLogged(this.context, entry.video, number, this.name, 'new');
    await announceBackStarted(this.context, entry.video, number, this.name, 'new');
  }
}

/** TikTok: заготовленный элемент пула получает новый MediaSource ролика 1. */
class RecreatedBackPool extends PoolEdge {
  stepBack(from, to) {
    this.queue = this.queue
      .then(() => this.#stepBack(to))
      .catch((error) => this.context.check('pool-step-back-completed', false, `${error?.name}: ${error?.message}`));
  }

  async #stepBack(to) {
    const retired = this.active;
    const retiredReel = retired.reel;
    const fresh = this.standby;
    await fresh.ready;
    // Заготовленный элемент переезжает в секцию ролика 1 (скрытым) и получает новый MediaSource ролика 1.
    this.#stage(fresh, to);
    await fresh.player.newGeneration({ refs: this.source.refsOf(to), positionS: 0, autoplay: false });
    fresh.reel = to;
    this.context.log(`stimulus reel-element-recreated n=${to}`);
    fresh.video.style.visibility = 'visible';
    retired.video.remove();
    retired.video.pause();
    await playForBack(this.context, fresh.video, to, this.name, 'new');
    this.active = fresh;
    await announceBackStarted(this.context, fresh.video, to, this.name, 'new');
    // Освободившийся элемент снова готовит ролик, который лента покажет следом (как на шаге вперёд).
    retired.reel = retiredReel;
    retired.ready = retired.player
      .newGeneration({ refs: this.source.refsOf(retiredReel), positionS: 0, autoplay: false })
      .then(() => this.#stage(retired, retiredReel));
    this.standby = retired;
  }

  /** Кладёт заранее загруженный ролик в секцию, но не показывает. */
  #stage(entry, number) {
    entry.video.style.visibility = 'hidden';
    this.feed.sectionOf(number).append(entry.video);
  }
}

/**
 * TikTok: «Назад» в истории после ухода из ленты (16b3b, решения 375-376). Форма вперёд и уход — прежние
 * (`PoolEdge`). Хук `history-back`: страница делает `history.back()` и по `popstate` возвращает в документ ТОТ ЖЕ
 * элемент ролика, на котором был уход (тот же `<video>`, тот же MediaSource), ставит его на 0 и играет; тихое
 * превью раздела, куда ушли, останавливается и убирается.
 */
class HistoryBackPool extends PoolEdge {
  historyBack() {
    window.addEventListener('popstate', () => {
      this.#restore().catch((error) => this.context.check('history-back-completed', false, `${error?.name}: ${error?.message}`));
    }, { once: true });
    history.back();
  }

  async #restore() {
    this.context.log('history-back-popstate');
    // Превью другого раздела уходит: страница показывала его вместо ленты.
    const preview = this.standby;
    preview.video.pause();
    preview.video.remove();
    const entry = this.active;
    const number = entry.reel;
    this.feed.sectionOf(number).append(entry.video);
    entry.video.style.visibility = 'visible';
    entry.video.currentTime = 0;
    await playForBack(this.context, entry.video, number, this.name, 'same');
    await announceBackStarted(this.context, entry.video, number, this.name, 'same');
  }
}

/** Строит форму с возвратом `back` (`live` или `recreated`) для формы ленты `shape`. */
export function createBackShape(shape, back, context, source, feed, options) {
  if (shape === 'many' && back === BACK_LIVE) return new LiveBackMany(context, source, feed, options);
  if (shape === 'many' && back === BACK_RECREATED) return new RecreatedBackMany(context, source, feed, options);
  if (shape === 'pool' && back === BACK_RECREATED) return new RecreatedBackPool(context, source, feed, options);
  if (shape === 'pool' && back === BACK_HISTORY) return new HistoryBackPool(context, source, feed, options);
  throw new Error(`bad page query: back=${back} is not modelled for shape=${shape}`);
}
