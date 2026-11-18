// «Страница качает дольше, чем помнит мост» (сессия 16b2, находка живой проверки Instagram: «после прокрутки
// нет мгновенного воспроизведения ролика»).
//
// Что было на живом сайте. При открытии лента качает НАЧАЛО трёх следующих роликов и только потом докачивает
// текущий. Мост держит ссылки на скачанное страницей («память без копий», потолок 64 МиБ на документ), чтобы
// при переезде окна отдать плееру уже скачанное: второй раз страница его не пришлёт. Когда документ доходил до
// потолка, мост выкидывал самые старые записи документа, то есть как раз начала следующих роликов. После
// прокрутки плеер получал следующий ролик без начала, 10 с ждал данных у нуля и вставал посреди ролика.
//
// Что делает стенд. `EarlierReel` — ролик, MediaSource которого страница открыла РАНЬШЕ роликов ленты (так на
// живой ленте стоит текущий ролик относительно следующих). По команде робота, когда начала следующих роликов
// уже скачаны, он качает больше потолка моста. Правильный мост берёт память у него, самого старого MediaSource
// документа, и следующий ролик уходит плееру с нуля.
//
// Элемент ролика в документ не вставляется: это ролик, который лента уже пролистала, показывать его негде, а
// MediaSource открывается и у элемента вне документа.

import { resolveRef } from './bench-player.js';

/** Параметр адреса страницы `history`: ничего или «скачать больше, чем помнит мост». */
export const HISTORY_NONE = 'none';
export const HISTORY_PAST_BRIDGE_MEMORY = 'past-bridge-memory';
export const HISTORY_MODES = [HISTORY_NONE, HISTORY_PAST_BRIDGE_MEMORY];

/** Имя хука страницы, которым робот запускает докачку (`hook:<имя>`). */
export const DOWNLOAD_HOOK = 'download-past-bridge-memory';

const MEBIBYTE = 1024 * 1024;
/**
 * Сколько ролик докачивает по команде: потолок моста 64 МиБ на документ (`DOCUMENT_MEMORY_BUDGET_BYTES`
 * экстеншена) и запас 16 МиБ, чтобы мосту пришлось отдать заметно больше, чем весят начала роликов ленты.
 */
const DOWNLOAD_BYTES = 80 * MEBIBYTE;
/** Размер одного `appendBuffer`: как крупный сегмент живого сайта (~1 МиБ), чтобы докачка шла секунды. */
const APPEND_BYTES = MEBIBYTE;
/** Какой ролик каталога качается (любой с видео в сегментах; берём самый лёгкий). */
const ASSET_ID = 'reel-3';
/** Сколько ждём `sourceopen` и конца одного `appendBuffer`, мс. */
const STEP_TIMEOUT_MS = 10000;

/** Ждёт событие `type` у `target` не дольше срока; срок вышел — ошибка с именем шага. */
function once(target, type, step) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`earlier reel: no '${type}' while ${step}`)), STEP_TIMEOUT_MS);
    target.addEventListener(type, () => { clearTimeout(timer); resolve(); }, { once: true });
  });
}

async function fetchBytes(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`earlier reel: ${url} answered ${response.status}`);
  return new Uint8Array(await response.arrayBuffer());
}

/** Склеивает `segment` сам с собой до `targetBytes`: подряд идущие одинаковые сегменты — законный поток MSE. */
function repeatedTo(segment, targetBytes) {
  const copies = Math.max(1, Math.ceil(targetBytes / segment.length));
  const joined = new Uint8Array(copies * segment.length);
  for (let copy = 0; copy < copies; copy += 1) joined.set(segment, copy * segment.length);
  return joined;
}

/** Ролик, открытый раньше роликов ленты (см. шапку файла). */
export class EarlierReel {
  #context;
  #sourceBuffer;
  #segmentUrl;
  // Элемент и MediaSource держим сами: без ссылок сборщик мусора закрыл бы MediaSource, и мост забыл бы его.
  #video;
  #mediaSource;

  constructor(context, video, mediaSource, sourceBuffer, segmentUrl) {
    this.#context = context;
    this.#video = video;
    this.#mediaSource = mediaSource;
    this.#sourceBuffer = sourceBuffer;
    this.#segmentUrl = segmentUrl;
  }

  /** Открывает MediaSource ролика и кладёт init-сегмент. Вызывать ДО роликов ленты: возраст решает. */
  static async open(context, catalog) {
    const asset = catalog.assets[ASSET_ID];
    const videoTrack = asset.tracks.find((track) => track.kind === 'video');
    const track = resolveRef(catalog, { assetId: ASSET_ID, trackId: videoTrack.id });
    const mediaUrl = (relative) => new URL(relative, `${location.origin}/media/`).href;
    const video = document.createElement('video');
    video.muted = true;
    const mediaSource = new MediaSource();
    const opened = once(mediaSource, 'sourceopen', 'opening the media source');
    video.src = URL.createObjectURL(mediaSource);
    await opened;
    const sourceBuffer = mediaSource.addSourceBuffer(track.mime);
    const reel = new EarlierReel(context, video, mediaSource, sourceBuffer, mediaUrl(track.segments[0].url));
    await reel.#append(await fetchBytes(mediaUrl(track.init)), 'appending the init segment');
    return reel;
  }

  async #append(bytes, step) {
    const updated = once(this.#sourceBuffer, 'updateend', step);
    this.#sourceBuffer.appendBuffer(bytes);
    await updated;
  }

  /**
   * Качает больше, чем помнит мост: каждый `appendBuffer` — отдельная загрузка (свой буфер), как у настоящего
   * плеера сайта. В конце пишет в журнал `earlier-reel-downloaded mib=<N>`: по ней робот знает, что докачка была.
   */
  async downloadPastBridgeMemory() {
    const piece = repeatedTo(await fetchBytes(this.#segmentUrl), APPEND_BYTES);
    let downloadedBytes = 0;
    while (downloadedBytes < DOWNLOAD_BYTES) {
      // `slice()` — новый буфер: мост считает память по буферам страницы, один и тот же он учёл бы один раз.
      await this.#append(piece.slice(), 'downloading past the bridge memory');
      downloadedBytes += piece.length;
    }
    this.#context.log(`earlier-reel-downloaded mib=${Math.round(downloadedBytes / MEBIBYTE)} source=${this.#mediaSource.readyState} element_paused=${this.#video.paused}`);
  }
}
