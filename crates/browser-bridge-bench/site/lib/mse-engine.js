// Движок подкачки сегментов в MediaSource — ядро «злого» мини-плеера стенда.
//
// Модуль не трогает DOM, поэтому его можно импортировать и в странице, и в Dedicated Worker
// (там MediaSource создаётся самим воркером). Всё, что нужно знать о воспроизведении, движок
// получает через `clock()` -> { currentTime, paused }.
//
// Поведение копирует настоящие плееры:
//  * держит буфер наперёд (`bufferAheadS`), на паузе почти перестаёт качать;
//  * seek: abort + remove и запрос сегментов с нового места;
//  * смена качества: init другой дорожки в тот же SourceBuffer (changeType, если кодек другой);
//  * «зависание» по сценарию (`hangAt`) для проверки сторожа.

/** Период опроса «нужно ли качать ещё», мс. */
const POLL_MS = 100;
/** Допуск при сравнении границ сегментов, с. */
const BOUNDARY_EPS_S = 0.01;
/** На паузе качаем только если запаса меньше этого (нужен для первого кадра), с. */
const PAUSED_MIN_AHEAD_S = 1;
/** Запас буфера наперёд по умолчанию, с. */
export const DEFAULT_BUFFER_AHEAD_S = 10;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Индекс первого сегмента, который заканчивается позже `timeS` (сегмент, содержащий момент). */
export function segmentIndexAt(track, timeS) {
  const index = track.segments.findIndex(
    (segment) => segment.start_s + segment.duration_s > timeS + BOUNDARY_EPS_S,
  );
  return index === -1 ? track.segments.length : index;
}

/** Индекс первого сегмента, начинающегося не раньше `timeS` (для смены качества на границе). */
function segmentIndexStartingFrom(track, timeS) {
  const index = track.segments.findIndex(
    (segment) => segment.start_s >= timeS - BOUNDARY_EPS_S,
  );
  return index === -1 ? track.segments.length : index;
}

/** Ключ «полосы»: у каждой дорожки свой SourceBuffer, muxed — один на всё. */
function laneKindOf(track) {
  return track.id === 'muxed' ? 'muxed' : track.kind;
}

export class MseEngine {
  /**
   * @param {object} options
   * @param {MediaSource} options.mediaSource уже открытый (sourceopen) MediaSource
   * @param {object[]} options.tracks дорожки каталога (по одной на SourceBuffer)
   * @param {number} options.durationS длительность ролика
   * @param {string} options.mediaBase абсолютный префикс URL медиа, например http://h/media/
   * @param {() => {currentTime:number, paused:boolean}} options.clock
   * @param {RequestCredentials} [options.credentials]
   * @param {number} [options.bufferAheadS]
   * @param {{kind:string, index:number}} [options.hangAt] перестать качать на сегменте
   * @param {number} [options.firstRequestDelayMs] не делать НИ ОДНОГО запроса (ни init, ни
   *   сегмента) первые N мс после `start()`: MediaSource открыт, SourceBuffer-ы добавлены, а
   *   данных ещё нет (медленный CDN при смене поколения)
   * @param {(line:string) => void} [options.log]
   */
  constructor(options) {
    this.ms = options.mediaSource;
    this.durationS = options.durationS;
    this.mediaBase = options.mediaBase;
    this.clock = options.clock;
    this.credentials = options.credentials ?? 'same-origin';
    this.bufferAheadS = options.bufferAheadS ?? DEFAULT_BUFFER_AHEAD_S;
    this.hangAt = options.hangAt ?? null;
    this.firstRequestDelayMs = options.firstRequestDelayMs ?? 0;
    /** Момент (performance.now), раньше которого запросов нет; задаётся в `start()`. */
    this.requestsAllowedAt = 0;
    this.log = options.log ?? (() => {});
    this.initialTracks = options.tracks;
    /** Журнал запросов: {kind:'init'|'segment', lane, url, startS, at}. */
    this.requests = [];
    this.lanes = [];
    this.epoch = 0;
    this.hung = false;
    this.endedStream = false;
    this.lastError = null;
  }

  /** Создаёт SourceBuffer-ы и запускает подкачку с позиции `positionS`. */
  start(positionS = 0) {
    this.ms.duration = this.durationS;
    if (this.firstRequestDelayMs > 0) {
      this.requestsAllowedAt = performance.now() + this.firstRequestDelayMs;
      this.log(`delaying the first request by ${this.firstRequestDelayMs} ms`);
    }
    this.lanes = this.initialTracks.map((track) => this.#createLane(track));
    this.#launchLoops(positionS);
  }

  /** Перемотка: старые запросы отменяются, лишнее удаляется, качаем с нового места. */
  async seek(positionS) {
    await this.#stopLoops();
    for (const lane of this.lanes) {
      await this.#repositionLane(lane, positionS);
    }
    this.endedStream = false;
    this.#launchLoops(positionS);
  }

  /** Просит перейти на другую дорожку той же «полосы»; применится на ближайшей границе сегмента. */
  switchTrack(track) {
    const lane = this.lanes.find((candidate) => candidate.kind === laneKindOf(track));
    if (!lane) throw new Error(`no lane for track ${track.id}`);
    lane.desiredTrack = track;
  }

  /**
   * «Выгрузка» живого эфира, как у Twitch на паузе: подкачка стоит, весь буфер каждой полосы
   * удалён (init остаётся в SourceBuffer — сам MediaSource жив). Дальше — `reload` или новый
   * MediaSource.
   */
  async unload() {
    await this.#stopLoops();
    for (const lane of this.lanes) {
      if (this.ms.readyState === 'open') {
        try { lane.sourceBuffer.abort(); } catch (error) { this.log(`abort failed: ${error}`); }
      }
      await this.#removeAll(lane.sourceBuffer);
      // Следующий запуск ставит полосу заново от своей позиции (а не продолжает старую).
      lane.initialised = false;
    }
    this.endedStream = false;
  }

  /** Загрузить заново с `positionS` в том же MediaSource (после `unload`). */
  reload(positionS) {
    this.#launchLoops(positionS);
  }

  /** Останавливает подкачку и ждёт завершения активных операций. */
  async stop() {
    await this.#stopLoops();
  }

  /** Сколько сегментных запросов сделано (init не считается). */
  segmentRequestCount() {
    return this.requests.filter((request) => request.kind === 'segment').length;
  }

  // ---- внутренности -------------------------------------------------------------------

  #createLane(track) {
    const sourceBuffer = this.ms.addSourceBuffer(track.mime);
    this.log(`source buffer added ${track.mime}`);
    return {
      kind: laneKindOf(track),
      sourceBuffer,
      track,
      desiredTrack: track,
      nextIndex: 0,
      appendedEnd: 0,
      bufferedFrom: 0,
      needInit: true,
      done: false,
      loop: null,
      abort: null,
    };
  }

  #launchLoops(positionS) {
    const epoch = ++this.epoch;
    this.hung = false;
    for (const lane of this.lanes) {
      if (!lane.initialised) this.#placeLane(lane, positionS);
      lane.done = false;
      lane.loop = this.#runLane(lane, epoch);
    }
  }

  /** Ставит позицию подкачки полосы на сегмент, содержащий `positionS`. */
  #placeLane(lane, positionS) {
    lane.initialised = true;
    lane.nextIndex = segmentIndexAt(lane.track, positionS);
    const segment = lane.track.segments[lane.nextIndex];
    lane.appendedEnd = segment ? segment.start_s : this.durationS;
    lane.bufferedFrom = lane.appendedEnd;
  }

  async #stopLoops() {
    this.epoch += 1; // старые циклы увидят смену эпохи и выйдут
    for (const lane of this.lanes) lane.abort?.abort();
    await Promise.all(this.lanes.map((lane) => lane.loop));
  }

  #alive(epoch) {
    return epoch === this.epoch && this.ms.readyState !== 'closed';
  }

  async #repositionLane(lane, positionS) {
    const insideBuffered =
      positionS >= lane.bufferedFrom - BOUNDARY_EPS_S && positionS < lane.appendedEnd;
    if (insideBuffered) return; // нужные данные уже лежат в буфере
    const { sourceBuffer } = lane;
    if (this.ms.readyState === 'open') {
      try { sourceBuffer.abort(); } catch (error) { this.log(`abort failed: ${error}`); }
    }
    await this.#removeAll(sourceBuffer);
    this.#placeLane(lane, positionS);
  }

  async #removeAll(sourceBuffer) {
    if (sourceBuffer.buffered.length === 0) return;
    await this.#waitUpdateEnd(sourceBuffer, () => sourceBuffer.remove(0, Infinity));
  }

  /** Выполняет операцию над SourceBuffer и ждёт `updateend` (ошибка операции — исключение). */
  #waitUpdateEnd(sourceBuffer, operation) {
    return new Promise((resolve, reject) => {
      const onEnd = () => { cleanup(); resolve(); };
      const onError = () => { cleanup(); reject(new Error('SourceBuffer error event')); };
      const cleanup = () => {
        sourceBuffer.removeEventListener('updateend', onEnd);
        sourceBuffer.removeEventListener('error', onError);
      };
      sourceBuffer.addEventListener('updateend', onEnd);
      sourceBuffer.addEventListener('error', onError);
      try {
        operation();
      } catch (error) {
        cleanup();
        reject(error);
      }
    });
  }

  async #runLane(lane, epoch) {
    try {
      // Задержка первых данных: до неё поток «открыт, но пуст». Смена эпохи её прерывает.
      while (this.#alive(epoch) && performance.now() < this.requestsAllowedAt) await sleep(POLL_MS);
      while (this.#alive(epoch)) {
        if (lane.track !== lane.desiredTrack) await this.#switchLaneTrack(lane);
        if (lane.needInit) await this.#appendInit(lane, epoch);
        if (!this.#alive(epoch)) break;
        const segment = lane.track.segments[lane.nextIndex];
        if (!segment) {
          lane.done = true;
          await this.#finishStreamIfAllDone(epoch);
          break;
        }
        if (this.#shouldHang(lane)) { await sleep(POLL_MS); continue; }
        if (!this.#wantsMore(lane)) { await sleep(POLL_MS); continue; }
        await this.#fetchAndAppendSegment(lane, segment, epoch);
      }
    } catch (error) {
      this.lastError = error;
      this.log(`lane ${lane.kind} failed: ${error?.name ?? ''} ${error?.message ?? error}`);
    }
  }

  /** Запас буфера: с паузой качаем только минимум, без паузы — `bufferAheadS`. */
  #wantsMore(lane) {
    const { currentTime, paused } = this.clock();
    const aheadS = lane.appendedEnd - currentTime;
    return aheadS < (paused ? PAUSED_MIN_AHEAD_S : this.bufferAheadS);
  }

  /** Сценарий «зависания»: полоса вида `hangAt.kind` не качает, дойдя до сегмента `index`. */
  #shouldHang(lane) {
    const hang = this.hangAt;
    if (!hang || hang.kind !== lane.kind || lane.nextIndex < hang.index) return false;
    if (!this.hung) this.log(`hanging ${lane.kind} at segment ${lane.nextIndex}`);
    this.hung = true;
    return true;
  }

  async #appendInit(lane, epoch) {
    const bytes = await this.#fetchBytes(lane, lane.track.init, 'init', 0, epoch);
    if (!bytes || !this.#alive(epoch)) return;
    await this.#waitUpdateEnd(lane.sourceBuffer, () => lane.sourceBuffer.appendBuffer(bytes));
    lane.needInit = false;
  }

  /** Смена качества: changeType при другом кодеке, новый init, продолжение с той же границы. */
  async #switchLaneTrack(lane) {
    const next = lane.desiredTrack;
    if (next.mime !== lane.track.mime) lane.sourceBuffer.changeType(next.mime);
    this.log(`switch ${lane.track.id} -> ${next.id} at ${lane.appendedEnd.toFixed(2)} s`);
    lane.track = next;
    lane.nextIndex = segmentIndexStartingFrom(next, lane.appendedEnd);
    lane.needInit = true;
  }

  async #fetchAndAppendSegment(lane, segment, epoch) {
    const bytes = await this.#fetchBytes(lane, segment.url, 'segment', segment.start_s, epoch);
    if (!bytes || !this.#alive(epoch)) return;
    await this.#waitUpdateEnd(lane.sourceBuffer, () => lane.sourceBuffer.appendBuffer(bytes));
    lane.appendedEnd = segment.start_s + segment.duration_s;
    lane.nextIndex += 1;
  }

  /** Качает файл; `null`, если запрос отменён сменой эпохи (seek/stop). */
  async #fetchBytes(lane, relativeUrl, kind, startS, epoch) {
    const controller = new AbortController();
    lane.abort = controller;
    const url = new URL(relativeUrl, this.mediaBase).href;
    this.requests.push({ kind, lane: lane.kind, url, startS, at: performance.now() });
    try {
      const response = await fetch(url, { credentials: this.credentials, signal: controller.signal });
      if (!response.ok) throw new Error(`HTTP ${response.status} for ${url}`);
      return await response.arrayBuffer();
    } catch (error) {
      if (epoch !== this.epoch || error?.name === 'AbortError') return null;
      throw error;
    }
  }

  /** Когда все полосы дочитали — сообщаем MediaSource, что данных больше не будет. */
  async #finishStreamIfAllDone(epoch) {
    if (!this.lanes.every((lane) => lane.done)) return;
    while (this.lanes.some((lane) => lane.sourceBuffer.updating)) await sleep(POLL_MS);
    if (this.#alive(epoch) && this.ms.readyState === 'open' && !this.endedStream) {
      this.endedStream = true;
      this.ms.endOfStream();
      this.log('end of stream');
    }
  }
}
