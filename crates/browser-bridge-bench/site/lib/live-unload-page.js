// Общее тело страниц «живой эфир, как Twitch» (решение 194, дефект 1 сессии 09b2).
//
// Что повторяет страница (подсмотрено записью живого Twitch в Firefox и Chromium):
//  * ставит обработчики Media Session `play` / `pause` и `metadata` — как Twitch;
//  * на свою паузу (обработчик `pause` или событие `pause` элемента, пока эфир «загружен»)
//    «выгружает» эфир: `pause()`, удаляет ВЕСЬ буфер, `currentTime = 0`, состояние «выгружен»;
//  * в состоянии «выгружен» на событие `play`, пришедшее НЕ из её обработчика, тут же снова
//    зовёт `pause()` (машина состояний плеера Twitch не умеет оживать от события элемента);
//  * её обработчик `play` загружает эфир заново с 0: в том же MediaSource (`same-media-source`)
//    или новым MediaSource на том же элементе (`new-media-source`; в воркере — только так).
//
// Ограничение стенда: временная шкала конечная (ролик 30 с, `duration` = 30), а не живая
// (`Infinity` + `setLiveSeekableRange`): мини-плеер стенда живой шкалы не умеет. Механика
// «выгрузка → загрузка с нуля» от этого не меняется.
//
// Режим передачи: робот шлёт `hook:expect-unloaded` после паузы плеера и `hook:expect-reloaded`
// после play плеера. Обычный режим (без экстеншена): страница сама нажимает свои «медиаклавиши»
// и проверяет, что выгрузка, повторная пауза на голый `play` и загрузка работают.

import { runScenario, UnsupportedError } from './bench-report.js';
import { checkPlaying, sleep, waitFor } from './bench-checks.js';
import { observeHandoff, startAsset } from './scenario-kit.js';

/** Буфер наперёд, с (как у mse-fmp4: короткий, ролик всего 30 с). */
const BUFFER_AHEAD_S = 4;
/** Сколько ждём выгрузку после паузы плеера, мс. */
const UNLOAD_WAIT_MS = 8000;
/** Сколько ждём загрузку и игру после play плеера, мс. */
const RELOAD_WAIT_MS = 10000;
/** На сколько секунд часы страницы обязаны уйти после загрузки: «играет», а не «стоит». */
const CLOCK_ADVANCE_S = 0.5;
/** Сколько ждём, что страница снова встала на паузу после голого `play` элемента, мс. */
const REPAUSE_WAIT_MS = 2000;

/** Как страница загружает эфир заново. */
export const LIVE_RELOAD = Object.freeze({
  SAME_MEDIA_SOURCE: 'same-media-source',
  NEW_MEDIA_SOURCE: 'new-media-source',
});

/** Состояния эфира страницы. */
const STATE = Object.freeze({
  LOADED: 'loaded',
  UNLOADING: 'unloading',
  UNLOADED: 'unloaded',
  RELOADING: 'reloading',
});

/** Плеер сайта «как Twitch»: выгрузка на паузе, загрузка только из обработчика Media Session. */
class LiveUnloadMimic {
  constructor(context, video, player, reload) {
    this.context = context;
    this.video = video;
    this.player = player;
    this.reload = reload;
    this.state = STATE.LOADED;
    /** Чем вызвана каждая выгрузка: `media-session-pause` или `element-pause`. */
    this.unloadCauses = [];
    /** Сколько раз эфир загружен заново. */
    this.reloads = 0;
    /** Сколько раз страница снова встала на паузу на голый `play` в состоянии «выгружен». */
    this.repauses = 0;
    this.handlers = {
      pause: () => {
        context.log('media-session pause handler');
        this.#unload('media-session-pause');
      },
      play: () => {
        context.log('media-session play handler');
        this.#reload();
      },
    };
  }

  /** Ставит обработчики Media Session и слушатели элемента (как плеер Twitch при загрузке). */
  install() {
    navigator.mediaSession.metadata = new MediaMetadata({ title: 'bench live stream' });
    navigator.mediaSession.setActionHandler('pause', this.handlers.pause);
    navigator.mediaSession.setActionHandler('play', this.handlers.play);
    this.video.addEventListener('pause', () => {
      // Своя пауза из `#unload` приходит уже в состоянии «выгружается»; конец ролика — не пауза зрителя.
      if (this.state === STATE.LOADED && !this.video.ended) this.#unload('element-pause');
    });
    this.video.addEventListener('play', () => {
      if (this.state !== STATE.UNLOADED) return;
      this.repauses += 1;
      this.context.log('play event while unloaded: pausing again (like Twitch)');
      this.video.pause();
    });
  }

  async #unload(cause) {
    if (this.state !== STATE.LOADED) return;
    this.state = STATE.UNLOADING;
    this.video.pause();
    await this.player.unloadLive();
    this.video.currentTime = 0;
    this.unloadCauses.push(cause);
    this.state = STATE.UNLOADED;
    this.context.log(`unloaded (${cause})`);
  }

  async #reload() {
    if (this.state !== STATE.UNLOADED) {
      this.context.log(`play handler ignored in state ${this.state}`);
      return;
    }
    this.state = STATE.RELOADING;
    try {
      if (this.reload === LIVE_RELOAD.NEW_MEDIA_SOURCE) await this.player.reloadLiveWithNewSource();
      else this.player.reloadLiveInPlace();
      await this.player.play();
      this.reloads += 1;
      this.state = STATE.LOADED;
      this.context.log(`reloaded (${this.reload})`);
    } catch (error) {
      this.context.log(`reload failed: ${error?.name ?? ''} ${error?.message ?? error}`);
      this.state = STATE.UNLOADED;
    }
  }

  /** Строка состояния для деталей проверок. */
  describe() {
    const buffered = this.video.buffered.length > 0 ? this.video.buffered.end(this.video.buffered.length - 1).toFixed(2) : 'empty';
    return `state=${this.state}, unloads=[${this.unloadCauses.join(',')}], reloads=${this.reloads}, repauses=${this.repauses}, `
      + `paused=${this.video.paused}, currentTime=${this.video.currentTime.toFixed(2)}, buffered end=${buffered}`;
  }

  /** Проверка «выгружен» (после паузы плеера или своей медиаклавиши). */
  async expectUnloaded(context, name) {
    const unloaded = await waitFor(() => this.state === STATE.UNLOADED, UNLOAD_WAIT_MS);
    context.check(name, unloaded && this.video.paused && this.video.currentTime < 1, this.describe());
  }

  /** Проверка «загружен заново и играет»: состояние, игра и ход часов страницы. */
  async expectReloaded(context, name) {
    const reloaded = await waitFor(() => this.state === STATE.LOADED && this.reloads > 0 && !this.video.paused, RELOAD_WAIT_MS);
    const before = this.video.currentTime;
    const advanced = reloaded && await waitFor(() => this.video.currentTime >= before + CLOCK_ADVANCE_S, RELOAD_WAIT_MS);
    context.check(name, reloaded && advanced, `${this.describe()}, clock from ${before.toFixed(2)}`);
  }

  /** Обычный режим: свои «медиаклавиши» и причуда «голый play в выгруженном → снова пауза». */
  async selfCheck(context) {
    this.handlers.pause();
    await this.expectUnloaded(context, 'unloads-on-pause-key');
    // Голый `play` элемента (не из обработчика) эфир не оживляет: страница снова на паузе.
    this.video.play().catch(() => {});
    const repaused = await waitFor(() => this.repauses > 0, REPAUSE_WAIT_MS);
    await sleep(200);
    context.check('bare-element-play-is-paused-again', repaused && this.video.paused && this.state === STATE.UNLOADED, this.describe());
    this.handlers.play();
    await this.expectReloaded(context, 'reloads-on-play-key');
    await checkPlaying(context, this.video, 'playing-after-reload');
  }
}

/**
 * @param {string} scenarioName имя страницы (= имя файла без `.html`)
 * @param {{reload: string, worker?: false | 'classic'}} variant
 */
export async function runLiveUnloadPage(scenarioName, variant) {
  await runScenario(scenarioName, async (ctx) => {
    if (variant.worker && !MediaSource.canConstructInDedicatedWorker) {
      throw new UnsupportedError(`MediaSource.canConstructInDedicatedWorker=${MediaSource.canConstructInDedicatedWorker}`);
    }
    if (!('mediaSession' in navigator)) throw new Error('navigator.mediaSession is missing');
    const session = await startAsset(ctx, 'h264-aac', { bufferAheadS: BUFFER_AHEAD_S, worker: variant.worker ?? false });
    const { video, player } = session;
    await checkPlaying(ctx, video, 'playing');
    const mimic = new LiveUnloadMimic(ctx, video, player, variant.reload);
    mimic.install();
    if (!ctx.params.handoff) {
      await mimic.selfCheck(ctx);
      await player.destroy();
      return;
    }
    const newSource = variant.reload === LIVE_RELOAD.NEW_MEDIA_SOURCE;
    await observeHandoff(ctx, session, {
      'expect-unloaded': (hookCtx) => mimic.expectUnloaded(hookCtx, 'player-pause-unloads-the-live-stream'),
      'expect-reloaded': (hookCtx) => mimic.expectReloaded(hookCtx, 'player-play-reloads-the-live-stream'),
    }, {
      expectedExtraGenerations: newSource ? 1 : 0,
      // Загрузка в том же MediaSource начинает шкалу с 0: часы и конец буфера страницы законно
      // меньше, чем при передаче. Ход часов после загрузки проверяет сам `expect-reloaded`.
      skipFinishChecks: newSource ? [] : ['page-clock-advances', 'page-buffer-keeps-filling'],
    });
    await player.destroy();
  });
}
