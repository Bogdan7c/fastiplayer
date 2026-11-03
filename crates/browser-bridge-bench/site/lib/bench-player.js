// Мини-плеер стенда: ведёт себя как «злые» настоящие MSE-плееры (см. mse-engine.js).
//
// API (всё, что нужно сценариям):
//   const catalog = await loadCatalog();
//   const player = new BenchPlayer(video, catalog, { bufferAheadS, mediaBase, credentials,
//                                                    stallSeconds, hangAt, worker, log });
//   worker: true | 'module' — MSE в модульном Dedicated Worker, 'classic' — в классическом (Twitch);
//   await player.start({ refs: standardRefs(catalog, 'h264-aac'), positionS: 0 });
//   await player.seek(20);                       // abort/remove + запросы с нового места
//   player.switchTrack({assetId, trackId});      // ABR: init другой дорожки в тот же SourceBuffer
//   await player.newGeneration({ refs, positionS, releaseOldSource, firstRequestDelayMs });
//                                                // новый MediaSource на ТОМ ЖЕ <video>;
//                                                // releaseOldSource — как снять старый источник
//                                                // (RELEASE_OLD_SOURCE), firstRequestDelayMs —
//                                                // задержка первых данных нового поколения
//   await player.unloadLive();                   // «выгрузка» эфира, как Twitch на паузе
//   player.reloadLiveInPlace();                  // снова качать с 0 в том же MediaSource
//   await player.reloadLiveWithNewSource();      // снова с 0, новым MediaSource
//   player.generations                           // сколько MediaSource создано на элементе
//   player.requests                              // журнал сегментных запросов по всем поколениям
//   await player.destroy();
// Сторож зависания включается параметром stallSeconds: нет прироста currentTime при !paused —
// пересоздаём MediaSource на том же элементе с текущей позиции.

import { DEFAULT_BUFFER_AHEAD_S, MseEngine } from './mse-engine.js';
import { fetchJson } from './bench-checks.js';

/** Префикс медиа по умолчанию (для /cdn/ сценарий передаёт свой). */
const DEFAULT_MEDIA_PATH = '/media/';
/** Период проверки сторожа, мс. */
const WATCHDOG_PERIOD_MS = 500;
/** Минимальный сдвиг currentTime, считающийся прогрессом, с. */
const PROGRESS_EPS_S = 0.05;
/** Период пересылки часов воркеру, мс. */
const WORKER_CLOCK_PERIOD_MS = 100;
/** Сколько ждём sourceopen / handle от воркера, мс. */
const OPEN_TIMEOUT_MS = 5000;

export async function loadCatalog(origin = location.origin) {
  return fetchJson(`${origin}/media/catalog.json`);
}

/** Дорожки обычного сегментированного ролика: видео (выбранное или первое) + звук, либо muxed. */
export function standardRefs(catalog, assetId, videoTrackId = null) {
  const asset = catalog.assets[assetId];
  if (!asset?.tracks) throw new Error(`asset ${assetId} has no tracks`);
  const muxed = asset.tracks.find((track) => track.id === 'muxed');
  if (muxed) return [{ assetId, trackId: 'muxed' }];
  const video = videoTrackId
    ? asset.tracks.find((track) => track.id === videoTrackId)
    : asset.tracks.find((track) => track.kind === 'video');
  const audio = asset.tracks.find((track) => track.kind === 'audio');
  const refs = [{ assetId, trackId: video.id }];
  if (audio) refs.push({ assetId, trackId: audio.id });
  return refs;
}

/** Звук из другого ассета (озвучка) вместо родного. */
export function refsWithDub(catalog, assetId, dubAssetId, videoTrackId = null) {
  const videoOnly = standardRefs(catalog, assetId, videoTrackId).filter(
    (ref) => ref.trackId !== 'audio',
  );
  const dubTrack = catalog.assets[dubAssetId].tracks.find((track) => track.kind === 'audio');
  return [...videoOnly, { assetId: dubAssetId, trackId: dubTrack.id }];
}

export function resolveRef(catalog, ref) {
  return catalog.assets[ref.assetId].tracks.find((track) => track.id === ref.trackId);
}

/**
 * Как страница снимает старый источник перед новым MediaSource (так делают разные плееры):
 *  * `src` — просто присваивает новый `src` (как было всегда);
 *  * `srcObject-null` — сначала `srcObject = null` (YouTube перед рекламой), потом `src`;
 *  * `remove-attr-load` — `removeAttribute('src'); load()`, потом `src`.
 */
export const RELEASE_OLD_SOURCE = Object.freeze({
  SRC: 'src',
  SRC_OBJECT_NULL: 'srcObject-null',
  REMOVE_ATTR_LOAD: 'remove-attr-load',
});

const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

export class BenchPlayer {
  constructor(video, catalog, options = {}) {
    this.video = video;
    this.catalog = catalog;
    this.bufferAheadS = options.bufferAheadS ?? DEFAULT_BUFFER_AHEAD_S;
    this.mediaBase = new URL(options.mediaBase ?? DEFAULT_MEDIA_PATH, location.href).href;
    this.credentials = options.credentials ?? 'same-origin';
    this.stallSeconds = options.stallSeconds ?? null;
    this.hangAt = options.hangAt ?? null;
    this.useWorker = Boolean(options.worker ?? false);
    /** Вид воркера: классический (как Twitch) или модульный. */
    this.workerType = options.worker === 'classic' ? 'classic' : 'module';
    /** Handle от воркера, ещё не привязанные к элементу (по одному на поколение). */
    this.pendingHandles = [];
    this.handleWaiters = [];
    /** Blob-адрес классического воркера (держится, пока жив воркер, как у IVS). */
    this.workerBlobUrl = null;
    this.logLine = options.log ?? (() => {});
    this.generations = 0;
    this.mediaSources = [];
    this.refs = [];
    this.engine = null;
    this.worker = null;
    this.workerSegments = 0;
    this.workerInfo = null;
    this.retiredRequests = [];
    this.objectUrl = null;
    this.clockTimer = null;
    this.watchdogTimer = null;
    this.recreating = false;
    this.pageSeeking = false;
    /** Эфир «выгружен» (`unloadLive`): чужие перемотки не качают, пока его не загрузят снова. */
    this.liveUnloaded = false;
    this.lastProgress = { at: performance.now(), time: 0 };
    this.onGeneration = options.onGeneration ?? (() => {});
  }

  /** Журнал сегментных запросов за всё время (завершённые поколения + текущее). */
  get requests() {
    return [...this.retiredRequests, ...(this.engine?.requests ?? [])];
  }

  /** Число сегментных запросов (в режиме воркера — со слов воркера). */
  get segmentRequestCount() {
    if (this.useWorker) return this.workerSegments;
    return this.requests.filter((request) => request.kind === 'segment').length;
  }

  async start({ refs, positionS = 0, autoplay = true }) {
    this.refs = refs;
    await (this.useWorker ? this.#attachInWorker(positionS) : this.#attachInPage(positionS));
    if (autoplay) await this.play();
    if (this.stallSeconds !== null && !this.watchdogTimer) {
      this.watchdogTimer = setInterval(() => this.#watchdogTick(), WATCHDOG_PERIOD_MS);
    }
  }

  /** Новое поколение: новый MediaSource на том же <video> (реклама, серия, озвучка, сторож). */
  async newGeneration({
    refs = this.refs,
    positionS = 0,
    // Вычисляется ДО снятия старого источника: `srcObject = null` и `load()` ставят элемент
    // на паузу, и «играл ли он» после этого уже не узнать.
    autoplay = !this.video.paused,
    releaseOldSource = RELEASE_OLD_SOURCE.SRC,
    firstRequestDelayMs = 0,
  } = {}) {
    if (this.useWorker) {
      // Воркер сам останавливает старый движок и создаёт новый MediaSource; сюда — его handle.
      this.refs = refs;
      await this.#attachInWorker(positionS, { releaseOldSource });
      if (autoplay) await this.play();
      return;
    }
    await this.#detachEngine();
    this.hangAt = null; // «зависание» — одноразовый сбой первого поколения
    this.refs = refs;
    await this.#attachInPage(positionS, { releaseOldSource, firstRequestDelayMs });
    if (autoplay) await this.play();
  }

  async seek(positionS) {
    this.pageSeeking = true;
    this.video.currentTime = positionS;
    try {
      if (this.useWorker) {
        this.worker.postMessage({ type: 'seek', positionS });
      } else {
        await this.engine.seek(positionS);
      }
    } finally {
      this.pageSeeking = false;
    }
  }

  /**
   * Как настоящий плеер: на ЧУЖУЮ перемотку элемента (в режиме передачи её ставит тень по
   * команде плеера) подкачать сегменты с нового места. Собственные перемотки страницы
   * (`seek()`) обрабатываются там же и не удваиваются.
   */
  followPlayerSeeks() {
    this.video.addEventListener('seeking', () => {
      if (this.pageSeeking || this.liveUnloaded) return;
      if (this.useWorker) {
        this.worker?.postMessage({ type: 'seek', positionS: this.video.currentTime });
        return;
      }
      if (!this.engine) return;
      this.engine.seek(this.video.currentTime)
        .catch((error) => this.logLine(`follow seek failed: ${error}`));
    });
  }

  /**
   * «Выгрузка» живого эфира, как у Twitch на паузе: подкачка стоит, весь буфер удалён.
   * В воркере выгрузку делает сам воркер (по очереди с поколениями); ответа страница не ждёт.
   */
  async unloadLive() {
    this.liveUnloaded = true;
    if (this.useWorker) {
      this.worker?.postMessage({ type: 'unload' });
      return;
    }
    await this.engine?.unload();
  }

  /** Снова качать эфир с начала в ТОМ ЖЕ MediaSource (только MSE в странице). */
  reloadLiveInPlace() {
    if (this.useWorker) throw new Error('reloadLiveInPlace: the worker variant reloads with a new MediaSource');
    this.liveUnloaded = false;
    this.engine.reload(0);
  }

  /** Снова качать эфир с начала НОВЫМ MediaSource на том же элементе (как Twitch после выгрузки). */
  async reloadLiveWithNewSource() {
    this.liveUnloaded = false;
    await this.newGeneration({ positionS: 0, autoplay: false });
  }

  /** ABR: переключиться на другую дорожку той же полосы. */
  switchTrack(ref) {
    const track = resolveRef(this.catalog, ref);
    this.engine.switchTrack(track);
    this.refs = this.refs.map((current) =>
      resolveRef(this.catalog, current).kind === track.kind ? ref : current,
    );
  }

  async play() {
    try {
      await this.video.play();
    } catch (error) {
      if (error?.name !== 'NotAllowedError') throw error;
      this.logLine(`autoplay blocked (${error.name}), retrying muted`);
      this.video.muted = true;
      await this.video.play();
    }
  }

  pause() {
    this.video.pause();
  }

  async destroy() {
    clearInterval(this.watchdogTimer);
    clearInterval(this.clockTimer);
    this.watchdogTimer = null;
    await this.#detachEngine();
    this.worker?.terminate();
    this.worker = null;
    if (this.workerBlobUrl) URL.revokeObjectURL(this.workerBlobUrl);
    this.workerBlobUrl = null;
  }

  // ---- внутренности -------------------------------------------------------------------

  #clock() {
    return { currentTime: this.video.currentTime, paused: this.video.paused };
  }

  #durationS() {
    return this.catalog.assets[this.refs[0].assetId].duration_s;
  }

  async #detachEngine() {
    if (this.engine) {
      await this.engine.stop();
      this.retiredRequests.push(...this.engine.requests);
      this.engine = null;
    }
  }

  /** Создаёт MediaSource, привязывает к элементу и запускает движок. */
  async #attachInPage(positionS, { releaseOldSource = RELEASE_OLD_SOURCE.SRC, firstRequestDelayMs = 0 } = {}) {
    const mediaSource = new MediaSource();
    const previousUrl = this.objectUrl;
    this.objectUrl = URL.createObjectURL(mediaSource);
    this.#releaseOldSource(releaseOldSource);
    this.video.src = this.objectUrl;
    if (previousUrl) URL.revokeObjectURL(previousUrl);
    this.mediaSources.push(mediaSource);
    this.generations += 1;
    await this.#waitSourceOpen(mediaSource);
    if (positionS > 0) this.video.currentTime = positionS;
    this.engine = new MseEngine({
      mediaSource,
      tracks: this.refs.map((ref) => resolveRef(this.catalog, ref)),
      durationS: this.#durationS(),
      mediaBase: this.mediaBase,
      clock: () => this.#clock(),
      credentials: this.credentials,
      bufferAheadS: this.bufferAheadS,
      hangAt: this.hangAt,
      firstRequestDelayMs,
      log: this.logLine,
    });
    this.engine.start(positionS);
    this.logLine(`generation ${this.generations} started at ${positionS.toFixed(2)} s`);
    this.onGeneration(this.generations);
  }

  /** Снимает старый источник выбранным способом (см. `RELEASE_OLD_SOURCE`). */
  #releaseOldSource(how) {
    const { video } = this;
    if (how === RELEASE_OLD_SOURCE.SRC) return;
    if (how === RELEASE_OLD_SOURCE.SRC_OBJECT_NULL) {
      video.srcObject = null;
    } else if (how === RELEASE_OLD_SOURCE.REMOVE_ATTR_LOAD) {
      video.removeAttribute('src');
      video.load();
    } else {
      throw new Error(`unknown releaseOldSource '${how}'`);
    }
    this.logLine(`old source released: ${how}`);
  }

  #waitSourceOpen(mediaSource) {
    return new Promise((resolve, reject) => {
      if (mediaSource.readyState === 'open') return resolve();
      const timer = setTimeout(() => reject(new Error('sourceopen timeout')), OPEN_TIMEOUT_MS);
      mediaSource.addEventListener('sourceopen', () => { clearTimeout(timer); resolve(); }, { once: true });
    });
  }

  /**
   * MSE в Dedicated Worker: воркер владеет MediaSource, на элемент уходит MediaSourceHandle.
   * Первый вызов создаёт воркер (модульный или классический) и шлёт `init`; следующие —
   * `new_generation` (новый MediaSource в том же воркере, старый источник снимается как велено).
   */
  async #attachInWorker(positionS, { releaseOldSource = RELEASE_OLD_SOURCE.SRC } = {}) {
    if (!MediaSource.canConstructInDedicatedWorker) {
      throw new Error('MediaSource in Dedicated Worker is not available');
    }
    const firstGeneration = this.worker === null;
    if (firstGeneration) this.#createWorker();
    const handlePromise = this.#nextHandle();
    this.worker.postMessage({
      type: firstGeneration ? 'init' : 'new_generation',
      // Классическому воркеру — абсолютный адрес плеера (относительный там разрешился бы от blob-адреса).
      coreUrl: new URL('./bench-worker-core.js', import.meta.url).href,
      tracks: this.refs.map((ref) => resolveRef(this.catalog, ref)),
      durationS: this.#durationS(),
      mediaBase: this.mediaBase,
      credentials: this.credentials,
      bufferAheadS: this.bufferAheadS,
      positionS,
    });
    const handle = await handlePromise;
    if (!firstGeneration) this.#releaseOldSource(releaseOldSource);
    this.video.srcObject = handle;
    this.generations += 1;
    this.logLine(`worker generation ${this.generations} attached at ${positionS.toFixed(2)} s`);
    this.onGeneration(this.generations);
    if (firstGeneration) {
      this.clockTimer = setInterval(() => {
        this.worker?.postMessage({ type: 'clock', ...this.#clock() });
      }, WORKER_CLOCK_PERIOD_MS);
      this.worker.postMessage({ type: 'clock', ...this.#clock() });
    }
    await delay(0);
  }

  /**
   * Воркер создаётся из blob-адреса страницы — так делают настоящие MSE-плееры в воркере
   * (решение 192: экстеншен оборачивает только такие воркеры):
   *  - классический, как Twitch (IVS): blob с `importScripts('<абсолютный адрес>')`, blob-адрес
   *    страница держит, пока жив воркер;
   *  - модульный, как встроенные воркеры Vite / worker-loader: blob с `import '<абсолютный адрес>'`,
   *    а blob-адрес отпускается СРАЗУ после конструктора (`try { new Worker } finally { revoke }`).
   *    Без отсрочки revoke в экстеншене такой воркер не загрузился бы.
   */
  #createWorker() {
    const isClassic = this.workerType === 'classic';
    const scriptUrl = isClassic
      ? new URL('./bench-worker-classic.js', import.meta.url).href
      : new URL('./bench-worker.js', import.meta.url).href;
    const loaderText = isClassic ? `importScripts(${JSON.stringify(scriptUrl)});\n` : `import ${JSON.stringify(scriptUrl)};\n`;
    const blobUrl = URL.createObjectURL(new Blob([loaderText], { type: 'text/javascript' }));
    if (isClassic) {
      this.worker = new Worker(blobUrl);
      this.workerBlobUrl = blobUrl;
    } else {
      try {
        this.worker = new Worker(blobUrl, { type: 'module' });
      } finally {
        URL.revokeObjectURL(blobUrl);
      }
    }
    this.worker.addEventListener('message', (event) => {
      const message = event.data;
      if (message.type === 'handle') this.#deliverHandle(message.handle);
      if (message.type === 'progress') this.workerSegments = message.segments;
      if (message.type === 'info') this.workerInfo = message;
      if (message.type === 'log') this.logLine(`worker: ${message.line}`);
    });
    this.worker.addEventListener('error', (event) => this.logLine(`worker error: ${event.message}`));
  }

  #deliverHandle(handle) {
    const waiter = this.handleWaiters.shift();
    if (waiter) waiter(handle);
    else this.pendingHandles.push(handle);
  }

  /** Следующий handle от воркера (или ошибка по сроку). */
  #nextHandle() {
    if (this.pendingHandles.length > 0) return Promise.resolve(this.pendingHandles.shift());
    return new Promise((resolve, reject) => {
      const waiter = (handle) => { clearTimeout(timer); resolve(handle); };
      const timer = setTimeout(() => {
        this.handleWaiters = this.handleWaiters.filter((candidate) => candidate !== waiter);
        reject(new Error('worker handle timeout'));
      }, OPEN_TIMEOUT_MS);
      this.handleWaiters.push(waiter);
    });
  }

  #watchdogTick() {
    const { video } = this;
    const now = performance.now();
    const idle = video.paused || video.ended || video.seeking;
    if (idle || Math.abs(video.currentTime - this.lastProgress.time) > PROGRESS_EPS_S) {
      this.lastProgress = { at: now, time: video.currentTime };
      return;
    }
    if (now - this.lastProgress.at < this.stallSeconds * 1000 || this.recreating) return;
    this.recreating = true;
    this.logLine(`stall at ${video.currentTime.toFixed(2)} s, recreating MediaSource`);
    this.newGeneration({ positionS: video.currentTime })
      .catch((error) => this.logLine(`recreate failed: ${error}`))
      .finally(() => {
        this.recreating = false;
        this.lastProgress = { at: performance.now(), time: video.currentTime };
      });
  }
}
