// Три формы ленты коротких видео, снятые с живых сайтов в Firefox (сессия 16b2). Страница сама листает
// ролики по прокрутке, как настоящий сайт: следит за секциями (`ReelFeed`), а форма решает, что делать с
// элементами `<video>`, когда лента ушла на следующий ролик.
//
//  * `many`  (Instagram): у каждого ролика свой `<video>` и свой MediaSource. Ушедший с экрана ролик
//    страница ставит на паузу (`pause()`), меняет адрес (`history.pushState`), и через 80-400 мс запускает
//    следующий (беззвучный, `muted`). Отыгравший элемент убирается из документа через ~6 с после паузы.
//  * `moved` (YouTube Shorts): один `<video>`. По прокрутке страница ПЕРЕНОСИТ тот же элемент в секцию
//    следующего ролика (убрать и вставить в одной задаче) и даёт ему новый MediaSource. Паузы нет.
//  * `pool`  (TikTok): два `<video>` по очереди. Следующий загружен заранее; по прокрутке прежний убирается
//    из документа и ставится на паузу, заранее загруженный показывается и играет (со звуком). Адрес тот же.

import { REEL_COUNT, announceReelStarted, randomDelayMs, sleep } from './feed-kit.js';

/** Через сколько после паузы страница убирает отыгравший элемент (форма `many`), мс. */
export const REMOVE_AFTER_PAUSE_MS = 6000;
/** Пауза между паузой старого ролика и `play()` нового (форма `many`): живой замер 80-400 мс. */
const NEXT_PLAY_DELAY_RANGE_MS = [80, 400];

/** Имена форм из параметра адреса `shape`. */
export const SHAPES = ['many', 'moved', 'pool'];

/** Строит форму по имени. `feed` — `ReelFeed`, `source` — `ReelSource`. */
export function createShape(shape, context, source, feed) {
  switch (shape) {
    case 'many': return new ManyShape(context, source, feed);
    case 'moved': return new MovedShape(context, source, feed);
    case 'pool': return new PoolShape(context, source, feed);
    default: throw new Error(`unknown shape '${shape}'`);
  }
}

/** Instagram: свой элемент на каждый ролик. */
class ManyShape {
  name = 'many';
  waitUntilOldIsGone = true;

  constructor(context, source, feed) {
    this.context = context;
    this.source = source;
    this.feed = feed;
    /** Номер ролика -> Promise { video, player } (элемент готовится один раз). */
    this.entries = new Map();
  }

  #prepareOnce(number) {
    if (!this.entries.has(number)) {
      this.entries.set(number, this.source.prepare(number, { muted: true }).then((entry) => {
        this.feed.sectionOf(number).append(entry.video);
        return entry;
      }));
    }
    return this.entries.get(number);
  }

  async start() {
    const first = await this.#prepareOnce(1);
    await first.video.play();
    const playing = await announceReelStarted(this.context, first.video, 1, this.name);
    // Следующий ролик готовится заранее, как у живого сайта.
    this.#prepareOnce(2);
    return playing;
  }

  async onAdvance(from, to) {
    this.context.log(`reel-switch from=${from} to=${to} shape=${this.name} t=${this.context.elapsedMs()}`);
    const began = performance.now();
    const old = await this.#prepareOnce(from);
    // 1. Ролик уже ушёл с экрана целиком: страница ставит его на паузу.
    old.video.pause();
    // 2. Адрес ленты меняется без перезагрузки (тот же origin).
    history.pushState({ reel: to }, '', `/reels/${to}/`);
    setTimeout(() => {
      old.video.remove();
      old.player.destroy().catch((error) => this.context.log(`old reel destroy failed: ${error}`));
      this.context.log(`reel-removed n=${from}`);
    }, REMOVE_AFTER_PAUSE_MS);
    // 3. Через 80-400 мс после паузы запускается следующий ролик.
    const next = await this.#prepareOnce(to);
    const delay = randomDelayMs(...NEXT_PLAY_DELAY_RANGE_MS);
    await sleep(Math.max(0, delay - (performance.now() - began)));
    await next.video.play();
    await announceReelStarted(this.context, next.video, to, this.name);
    if (to < REEL_COUNT) this.#prepareOnce(to + 1);
  }

  /** Итоговые проверки обычного режима: прежний ролик остановлен, адрес сменился. */
  async checkAfter(number) {
    const old = await this.#prepareOnce(number - 1);
    this.context.check(`many-reel-${number - 1}-paused-by-page`, old.video.paused, `paused=${old.video.paused}`);
    this.context.check(`many-url-pushed-for-reel-${number}`, history.state?.reel === number, `state=${JSON.stringify(history.state)} path=${location.pathname}`);
  }
}

/** YouTube Shorts: один элемент переезжает по секциям, у него новый MediaSource. */
class MovedShape {
  name = 'moved';
  waitUntilOldIsGone = false;

  constructor(context, source, feed) {
    this.context = context;
    this.source = source;
    this.feed = feed;
    this.entry = null;
  }

  async start() {
    this.entry = await this.source.prepare(1, { muted: false });
    this.feed.sectionOf(1).append(this.entry.video);
    await this.entry.video.play();
    return announceReelStarted(this.context, this.entry.video, 1, this.name);
  }

  async onAdvance(from, to) {
    this.context.log(`reel-switch from=${from} to=${to} shape=${this.name} t=${this.context.elapsedMs()}`);
    const { video, player } = this.entry;
    // Перенос ОДНОЙ задачей (убрать и вставить без паузы между): иначе браузер поставил бы элемент на паузу.
    this.feed.sectionOf(to).append(video);
    await player.newGeneration({ refs: this.source.refsOf(to), positionS: 0, autoplay: true });
    await announceReelStarted(this.context, video, to, this.name);
  }

  async checkAfter(number) {
    const { video } = this.entry;
    this.context.check(`moved-same-element-in-reel-${number}`, video.parentElement === this.feed.sectionOf(number), `parent=${video.parentElement?.id}`);
    this.context.check(`moved-element-plays-reel-${number}`, !video.paused, `paused=${video.paused}`);
    this.context.check(`moved-generations-${number}`, this.entry.player.generations === number, `generations=${this.entry.player.generations}`);
  }
}

/** TikTok: два элемента по очереди, следующий загружен заранее. */
class PoolShape {
  name = 'pool';
  waitUntilOldIsGone = true;

  constructor(context, source, feed) {
    this.context = context;
    this.source = source;
    this.feed = feed;
    this.active = null;
    this.standby = null;
    /** Для проверки обычного режима: элемент, убранный из документа при последнем переходе. */
    this.lastRetired = null;
  }

  async start() {
    this.active = await this.source.prepare(1, { muted: false });
    this.feed.sectionOf(1).append(this.active.video);
    this.standby = await this.source.prepare(2, { muted: false });
    this.#stage(this.standby, 2);
    await this.active.video.play();
    return announceReelStarted(this.context, this.active.video, 1, this.name);
  }

  /** Кладёт заранее загруженный ролик в секцию, но не показывает. */
  #stage(entry, number) {
    entry.video.style.visibility = 'hidden';
    this.feed.sectionOf(number).append(entry.video);
  }

  async onAdvance(from, to) {
    this.context.log(`reel-switch from=${from} to=${to} shape=${this.name} t=${this.context.elapsedMs()}`);
    const retired = this.active;
    const fresh = this.standby;
    // Прежний элемент убирается из документа и ставится на паузу.
    retired.video.remove();
    retired.video.pause();
    this.lastRetired = retired;
    // Заранее загруженный показывается и играет (со звуком).
    fresh.video.style.visibility = 'visible';
    await fresh.video.play();
    this.active = fresh;
    await announceReelStarted(this.context, fresh.video, to, this.name);
    // Освободившийся элемент загружает ролик после следующего.
    if (to < REEL_COUNT) {
      await retired.player.newGeneration({ refs: this.source.refsOf(to + 1), positionS: 0, autoplay: false });
      this.#stage(retired, to + 1);
      this.standby = retired;
    }
  }

  async checkAfter(number) {
    const retired = this.lastRetired;
    this.context.check(`pool-reel-${number - 1}-element-left-the-document`, !retired.video.isConnected && retired.video.paused === true, `connected=${retired.video.isConnected} paused=${retired.video.paused}`);
    this.context.check(`pool-reel-${number}-element-plays`, !this.active.video.paused, `paused=${this.active.video.paused}`);
    this.context.check(`pool-url-unchanged`, history.state === null && !location.pathname.startsWith('/reels/'), `path=${location.pathname}`);
  }
}
