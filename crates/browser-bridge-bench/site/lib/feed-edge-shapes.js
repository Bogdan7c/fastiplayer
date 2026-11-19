// Формы ленты для краёв (сессия 16b3), снятые с живых сайтов 19 ноября 2026 (Firefox 160). Отличие от
// feed-shapes.js: шаг формы не ждёт предыдущего (см. feed-edge-feed.js), а у каждой формы есть сценарий
// УХОДА ИЗ ЛЕНТЫ в другой раздел и удаления текущего ролика страницей.
//
//  * `many` (Instagram /reels/): свой <video> и MediaSource на ролик. Обычный шаг (I1): прежний ролик
//    `pause()`, `pushState`, через 80-400 мс `play()` следующего. Шаг серии (I4): `play()` следующего сразу,
//    прежний (он промежуточный) получает `pause()` через 40-120 мс; невидимый ролик за следующим получает
//    `play()` и тут же `pause()` (I1).
//    Уход (I6): `pushState('/explore/')`, через 0,5 с ВСЕ элементы ленты удалены из документа, наш ролик
//    получает `pause()` УЖЕ ПОСЛЕ удаления, через ~3 с заиграл тихий ролик-файл.
//  * `pool` (TikTok /foryou): два <video> по очереди, адрес не меняется. Шаг (T1/T3): прежний убран из
//    документа и на паузе, заготовленный показан и играет, освободившийся грузит новый MediaSource.
//    Уход (T5): `pushState('/explore')`, через 6 мс наш элемент удалён и после этого на паузе, второй
//    элемент пула получил новый MediaSource и играет тихо и видимо.
//  * `moved` (YouTube Shorts): один <video> переезжает по секциям, на шаге два новых MediaSource (S1), паузы
//    нет, адрес меняется на каждом шаге. Уход (S4): `pushState('/')`, через 1,2 с элемент переставлен в
//    документе, через 1,45 с его источник снят (`emptied`, паузы нет), через 1,9 с тихий ролик на MSE.
//
// Журнал страницы:
//   `stimulus <что>` — раздражитель состоялся (стенд требует эти строки: без них строка прошла бы вхолостую);
//   `leave-url-pushed path=<путь>` и прочие метки ухода — см. каждый сценарий.

import { announceReelStarted, randomDelayMs } from './feed-kit.js';
import { playFilePreview, playMsePreview } from './feed-edge-previews.js';

/** Пауза между паузой прежнего ролика и `play()` следующего на обычном шаге (I1): живой замер 80-400 мс. */
const NEXT_PLAY_DELAY_RANGE_MS = [80, 400];
/** Через сколько после `play()` следующего прежний промежуточный ролик получает `pause()` (I4). */
const INTERMEDIATE_PAUSE_RANGE_MS = [40, 120];
/** Следующая секция въехала в окно настолько, что ролик серии считается уходящим с экрана. */
const LEAVING_NEXT_SHARE = 0.3;
/** Страница удаляет прежний элемент через столько мс после `play()` следующего (находка «удалён во время переезда»). */
const REMOVE_OLD_AFTER_PLAY_MS = 30;

/** Причуды страницы: ничего или «прежний элемент удаляется прямо во время переезда окна». */
export const QUIRK_NONE = 'none';
export const QUIRK_REMOVE_OLD_DURING_MOVE = 'remove-old-during-move';
export const QUIRKS = [QUIRK_NONE, QUIRK_REMOVE_OLD_DURING_MOVE];
export const EDGE_SHAPES = ['many', 'pool', 'moved'];

/** Адрес ролика в адресной строке: числа и длинные слова — подстановка идентификатора (F5). */
const reelAddress = {
  many: (to) => `/reels/DR${to}kQmZ7xa1/`,
  moved: (to) => `/shorts/Sh${to}aBcDeFgHiJ`,
};

/** Строит форму по имени. */
export function createEdgeShape(name, context, source, feed, options) {
  switch (name) {
    case 'many': return new ManyEdge(context, source, feed, options);
    case 'pool': return new PoolEdge(context, source, feed, options);
    case 'moved': return new MovedEdge(context, source, feed, options);
    default: throw new Error(`unknown edge shape '${name}'`);
  }
}

/** Игнорирует отказ `play()` (его прерывает следующий `pause()`), но пишет его в журнал. */
function playQuietly(context, video, what) {
  video.play().catch((error) => context.log(`play-rejected ${what}: ${error?.name}`));
}

/** Instagram: свой элемент на каждый ролик. */
class ManyEdge {
  name = 'many';
  // Прежний ролик ушёл с экрана целиком (доля 0), и только тогда страница его останавливает (I1).
  waitUntilOldIsGone = true;

  constructor(context, source, feed, { reels, quirk }) {
    this.context = context;
    this.source = source;
    this.feed = feed;
    this.reels = reels;
    this.quirk = quirk;
    /** Номер ролика -> { video, player }. Все готовы заранее: живая лента держит 11-20 MediaSource. */
    this.entries = new Map();
    this.currentReel = 1;
  }

  async start() {
    for (let number = 1; number <= this.reels; number += 1) {
      const entry = await this.source.prepare(number, { muted: true });
      this.feed.sectionOf(number).append(entry.video);
      this.entries.set(number, entry);
    }
    const first = this.entries.get(1);
    await first.video.play();
    return announceReelStarted(this.context, first.video, 1, this.name);
  }

  step(from, to, kind) {
    const old = this.entries.get(from);
    const next = this.entries.get(to);
    this.currentReel = to;
    if (kind === 'burst') {
      // I4: промежуточный ролик виден целиком, `play()` сразу; прежний (тоже промежуточный) вскоре на паузе.
      history.pushState({ reel: to }, '', reelAddress.many(to));
      this.#startNext(next, to, old, from);
      // Прежний ролик серии (он тоже промежуточный) к этому времени уже на паузе или получает её сейчас.
      setTimeout(() => old.video.pause(), randomDelayMs(...INTERMEDIATE_PAUSE_RANGE_MS));
      // I4: сам этот ролик тоже получает `pause()` через 40-120 мс, если лента уже уходит к следующему.
      setTimeout(() => this.#pauseIfLeaving(next, to), randomDelayMs(...INTERMEDIATE_PAUSE_RANGE_MS));
      this.#touchInvisible(to + 1);
    } else {
      // I1: прежний на паузе, адрес, `play()` следующего через 80-400 мс.
      old.video.pause();
      history.pushState({ reel: to }, '', reelAddress.many(to));
      setTimeout(() => {
        // Серия могла уйти дальше, пока шла пауза: запаздывающий `play()` не должен оживлять ролик,
        // который страница уже оставила.
        if (this.currentReel === to) this.#startNext(next, to, old, from);
      }, randomDelayMs(...NEXT_PLAY_DELAY_RANGE_MS));
    }
  }

  /**
   * `play()` следующего ролика; запуск считается, только когда он действительно заиграл. С причудой страницы
   * прежний элемент (`old`, ролик `from`) удаляется из документа через миг после этого `play()`.
   */
  #startNext(next, number, old, from) {
    playQuietly(this.context, next.video, `reel ${number}`);
    announceReelStarted(this.context, next.video, number, this.name);
    if (this.quirk !== QUIRK_REMOVE_OLD_DURING_MOVE) return;
    setTimeout(() => {
      old.video.remove();
      this.context.log(`stimulus old-removed-during-move n=${from}`);
    }, REMOVE_OLD_AFTER_PLAY_MS);
  }

  /** Ролик серии уходит с экрана (в окно уже въехал следующий): страница ставит его на паузу. */
  #pauseIfLeaving(entry, number) {
    if (number >= this.reels || this.feed.ratios[number + 1] <= LEAVING_NEXT_SHARE) return;
    entry.video.pause();
  }

  /** Невидимый ролик за следующим получает `play()` и тут же `pause()` (I1: доля 0). */
  #touchInvisible(number) {
    const entry = this.entries.get(number);
    if (!entry) return;
    playQuietly(this.context, entry.video, `invisible reel ${number}`);
    entry.video.pause();
  }

  /** Раздел без видео после последнего ролика: прежний ролик ушёл с экрана, страница его останавливает. */
  tail(from) {
    this.entries.get(from).video.pause();
    this.context.log(`tail-reel-paused n=${from}`);
  }

  /** Страница убирает текущий ролик из документа, адрес не меняется (находка F2). */
  removeCurrent() {
    this.entries.get(this.currentReel).video.remove();
    this.context.log(`stimulus current-reel-removed n=${this.currentReel}`);
  }

  /** Уход из ленты (I6): адрес, удаление всех элементов, пауза уже удалённого, позже тихий файл. */
  leaveSection() {
    history.pushState({}, '', '/explore/');
    this.context.log('leave-url-pushed path=/explore/');
    setTimeout(() => {
      for (const entry of this.entries.values()) entry.video.remove();
      this.context.log('leave-elements-removed all=true');
      // Наш ролик получает `pause()` уже вне документа: страница ставит на паузу то, что убрала.
      this.entries.get(this.currentReel).video.pause();
      this.context.log(`leave-paused-after-removal n=${this.currentReel}`);
    }, 500);
    setTimeout(() => playFilePreview(this.context, this.source.catalog), 3000);
  }

  /** Уход из раздела без видео в другой раздел с играющим превью (F5): ничего не удаляется. */
  leaveFromTail() {
    setTimeout(() => {
      history.pushState({}, '', '/explore/');
      this.context.log('leave-url-pushed path=/explore/');
    }, 100);
    setTimeout(() => playFilePreview(this.context, this.source.catalog), 400);
  }
}

/** TikTok: два элемента по очереди; адрес не меняется. */
class PoolEdge {
  name = 'pool';
  waitUntilOldIsGone = true;

  constructor(context, source, feed, { reels, quirk }) {
    this.context = context;
    this.source = source;
    this.feed = feed;
    this.reels = reels;
    this.quirk = quirk;
    this.active = null;
    this.standby = null;
    /** Шаги выполняются по одному: заготовленный элемент должен успеть загрузиться. */
    this.queue = Promise.resolve();
  }

  async start() {
    this.active = await this.source.prepare(1, { muted: false });
    this.active.reel = 1;
    this.feed.sectionOf(1).append(this.active.video);
    this.standby = await this.source.prepare(2, { muted: false });
    this.standby.reel = 2;
    this.#stage(this.standby, 2);
    await this.active.video.play();
    return announceReelStarted(this.context, this.active.video, 1, this.name);
  }

  /** Кладёт заранее загруженный ролик в секцию, но не показывает. */
  #stage(entry, number) {
    entry.video.style.visibility = 'hidden';
    this.feed.sectionOf(number).append(entry.video);
  }

  step(from, to, kind) {
    this.queue = this.queue
      .then(() => this.#step(to, kind))
      .catch((error) => this.context.check('pool-step-completed', false, `${error?.name}: ${error?.message}`));
  }

  async #step(to, kind) {
    const retired = this.active;
    // Номер ролика на прежнем элементе: ниже элемент перезаряжается, и `retired.reel` меняется.
    const retiredReel = retired.reel;
    const fresh = this.standby;
    await fresh.ready;
    // Серия могла перескочить ролик: у заготовленного не тот, что нужен, — новый MediaSource под нужный.
    if (fresh.reel !== to) {
      await fresh.player.newGeneration({ refs: this.source.refsOf(to), positionS: 0, autoplay: false });
      fresh.reel = to;
    }
    fresh.video.style.visibility = 'visible';
    if (this.quirk === QUIRK_REMOVE_OLD_DURING_MOVE) {
      // Прежний элемент убирается уже ПОСЛЕ `play()` следующего, посреди переезда окна.
      const started = fresh.video.play();
      setTimeout(() => {
        retired.video.remove();
        retired.video.pause();
        this.context.log(`stimulus old-removed-during-move n=${retiredReel}`);
      }, REMOVE_OLD_AFTER_PLAY_MS);
      await started;
    } else {
      retired.video.remove();
      retired.video.pause();
      await fresh.video.play();
    }
    this.active = fresh;
    const started = announceReelStarted(this.context, fresh.video, to, this.name);
    // Обычный шаг: освободившийся элемент перезаряжается, когда следующий ролик уже играет (в быстрой серии
    // сайт не ждёт и перезаряжает сразу: T1, два-три новых MediaSource на шаг).
    if (kind !== 'burst') await started;
    // Освободившийся элемент загружает ролик после следующего (новый MediaSource на старом элементе).
    if (to < this.reels) {
      retired.reel = to + 1;
      retired.ready = retired.player
        .newGeneration({ refs: this.source.refsOf(to + 1), positionS: 0, autoplay: false })
        .then(() => this.#stage(retired, to + 1));
      this.standby = retired;
    }
  }

  removeCurrent() {
    this.active.video.remove();
    this.context.log(`stimulus current-reel-removed n=${this.active.reel}`);
  }

  /** Уход из ленты (T5): адрес, через 6 мс наш элемент удалён и на паузе, второй элемент играет тихое превью. */
  leaveSection() {
    history.pushState({}, '', '/explore');
    this.context.log('leave-url-pushed path=/explore');
    setTimeout(() => {
      this.active.video.remove();
      this.active.video.pause();
      this.context.log(`leave-paused-after-removal n=${this.active.reel}`);
      // Второй элемент пула переиспользован: новый MediaSource, тихо, виден.
      Promise.resolve(this.standby.ready).then(() => playMsePreview(this.context, this.source, this.standby));
    }, 6);
  }
}

/** YouTube Shorts: один элемент переезжает по секциям. */
class MovedEdge {
  name = 'moved';
  waitUntilOldIsGone = false;

  constructor(context, source, feed) {
    this.context = context;
    this.source = source;
    this.feed = feed;
    this.entry = null;
    this.currentReel = 1;
    /** Переходы выполняются по одному: новый MediaSource ставится на элемент после предыдущего. */
    this.queue = Promise.resolve();
  }

  async start() {
    this.entry = await this.source.prepare(1, { muted: false });
    this.feed.sectionOf(1).append(this.entry.video);
    await this.entry.video.play();
    return announceReelStarted(this.context, this.entry.video, 1, this.name);
  }

  step(from, to) {
    this.currentReel = to;
    this.queue = this.queue
      .then(() => this.#move(to))
      .catch((error) => this.context.check('moved-step-completed', false, `${error?.name}: ${error?.message}`));
  }

  async #move(to) {
    const { video, player } = this.entry;
    // Перенос ОДНОЙ задачей (убрать и вставить без паузы между): иначе браузер поставил бы элемент на паузу.
    this.feed.sectionOf(to).append(video);
    history.pushState({ reel: to }, '', reelAddress.moved(to));
    // S1: на шаге два новых MediaSource подряд (сначала без запуска, потом настоящий).
    await player.newGeneration({ refs: this.source.refsOf(to), positionS: 0, autoplay: false });
    await player.newGeneration({ refs: this.source.refsOf(to), positionS: 0, autoplay: true });
    announceReelStarted(this.context, video, to, this.name);
  }

  removeCurrent() {
    this.entry.video.remove();
    this.context.log(`stimulus current-reel-removed n=${this.currentReel}`);
  }

  /** Уход из ленты (S4): адрес, перестановка в документе, снятие источника (`emptied`), позже тихий MSE-ролик. */
  leaveSection() {
    history.pushState({}, '', '/');
    this.context.log('leave-url-pushed path=/');
    const { video } = this.entry;
    setTimeout(() => {
      const section = video.parentElement;
      video.remove();
      section.append(video);
      this.context.log('leave-element-moved');
    }, 1200);
    setTimeout(() => {
      // Источник снят без паузы: элемент получает `emptied`.
      video.removeAttribute('src');
      video.load();
      this.context.log('leave-source-removed');
    }, 1450);
    setTimeout(() => playMsePreview(this.context, this.source), 1900);
  }
}
