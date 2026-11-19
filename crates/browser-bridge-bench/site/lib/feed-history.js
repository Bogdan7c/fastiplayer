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
// уже скачаны, он качает больше потолка моста. Правильный мост берёт память у него самого: его элемента в
// документе нет, он не играл, и показать его зрителю страница не может (решение 364; до него память отдавал
// «самый старый MediaSource», решение 363, и этот ролик был таким). Следующий ролик уходит плееру с нуля.
//
// Элемент ролика в документ не вставляется: это ролик, который лента уже пролистала, показывать его негде, а
// MediaSource открывается и у элемента вне документа.
//
// Второй режим, `ReelCrowd` — форма живого Instagram по замеру во вкладке владельца 19 ноября 2026 (находка 3,
// решение 364). Лента держит живыми около 20 MediaSource: начала роликов далеко вперёд. В потолок моста
// страница упирается почти сразу, а пройденные ролики сбрасывает пачками. После такого сброса самый старый
// живой MediaSource документа — текущий ролик, за ним следующий по ленте. Мост, который отдаёт память «по
// возрасту», выкидывает ровно их, и после прокрутки следующий ролик приходит в окно без начала. Правильный
// мост берёт память у того, что зритель увидит позже всех: у начал дальних роликов.
//
// Что делает стенд во втором режиме. До роликов ленты страница открывает «пройденные» ролики (их MediaSource
// старше всех). По команде робота, после передачи и до прокрутки, она отсоединяет их от элементов (MediaSource
// закрывается), открывает «дальние» ролики и по кругу качает их начала сверх потолка моста. Смотрели ли
// пройденные ролики, строке не важно: к моменту нехватки памяти их уже нет.

import { resolveRef } from './bench-player.js';

/** Параметр адреса страницы `history`: ничего или «скачать больше, чем помнит мост». */
export const HISTORY_NONE = 'none';
export const HISTORY_PAST_BRIDGE_MEMORY = 'past-bridge-memory';
export const HISTORY_PASSED_REELS_DETACHED = 'passed-reels-detached';
export const HISTORY_MODES = [HISTORY_NONE, HISTORY_PAST_BRIDGE_MEMORY, HISTORY_PASSED_REELS_DETACHED];

/** Имя хука страницы, которым робот запускает докачку (`hook:<имя>`). */
export const DOWNLOAD_HOOK = 'download-past-bridge-memory';
/** Имя хука второго режима: сбросить пройденные ролики и скачать начала дальних (`hook:<имя>`). */
export const CROWD_HOOK = 'detach-passed-reels-and-preload-far-ahead';

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
/** Второй режим: сколько роликов лента уже пролистала и сколько каждый успел скачать. */
const PASSED_REEL_COUNT = 3;
const PASSED_REEL_BYTES = 2 * MEBIBYTE;
/**
 * Второй режим: сколько дальних роликов страница грузит заранее и сколько весит начало каждого. Вместе не
 * меньше 80 МиБ (кусок докачки чуть больше мебибайта): больше потолка моста на 16 МиБ с лишним, а текущий и
 * следующий ролики ленты вдвоём весят около 4 МиБ. Мост, отдающий память по возрасту, обязан съесть их оба целиком.
 */
const FAR_REEL_COUNT = 8;
const FAR_REEL_BYTES = 10 * MEBIBYTE;

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

/** Ролик вне документа: свой `<video>`, свой MediaSource, один SourceBuffer с видео. */
class OffDocumentReel {
  #sourceBuffer;
  #segmentUrl;
  // Элемент и MediaSource держим сами: без ссылок сборщик мусора закрыл бы MediaSource, и мост забыл бы его.
  #video;
  #mediaSource;

  constructor(video, mediaSource, sourceBuffer, segmentUrl) {
    this.#video = video;
    this.#mediaSource = mediaSource;
    this.#sourceBuffer = sourceBuffer;
    this.#segmentUrl = segmentUrl;
  }

  /** Открывает MediaSource ролика и кладёт init-сегмент. `who` — имя ролика для текста ошибок. */
  static async open(catalog, who) {
    const asset = catalog.assets[ASSET_ID];
    const videoTrack = asset.tracks.find((track) => track.kind === 'video');
    const track = resolveRef(catalog, { assetId: ASSET_ID, trackId: videoTrack.id });
    const mediaUrl = (relative) => new URL(relative, `${location.origin}/media/`).href;
    const video = document.createElement('video');
    video.muted = true;
    const mediaSource = new MediaSource();
    const opened = once(mediaSource, 'sourceopen', `${who}: opening the media source`);
    video.src = URL.createObjectURL(mediaSource);
    await opened;
    const sourceBuffer = mediaSource.addSourceBuffer(track.mime);
    const reel = new OffDocumentReel(video, mediaSource, sourceBuffer, mediaUrl(track.segments[0].url));
    await reel.append(await fetchBytes(mediaUrl(track.init)), `${who}: appending the init segment`);
    return reel;
  }

  /** Один `appendBuffer` и ожидание его конца. */
  async append(bytes, step) {
    const updated = once(this.#sourceBuffer, 'updateend', step);
    this.#sourceBuffer.appendBuffer(bytes);
    await updated;
  }

  /** Кусок для докачки: первый медиасегмент ролика, склеенный сам с собой до размера одного `appendBuffer`. */
  async downloadPiece() {
    return repeatedTo(await fetchBytes(this.#segmentUrl), APPEND_BYTES);
  }

  /** Страница снимает источник с элемента: MediaSource закрывается (`readyState` = `closed`). */
  detach() {
    this.#video.removeAttribute('src');
    this.#video.load();
  }

  /** Состояние для журнала страницы. */
  get state() {
    return { source: this.#mediaSource.readyState, elementPaused: this.#video.paused };
  }
}

/** Ролик, открытый раньше роликов ленты (см. шапку файла). */
export class EarlierReel {
  #context;
  #reel;

  constructor(context, reel) {
    this.#context = context;
    this.#reel = reel;
  }

  /** Открывает MediaSource ролика и кладёт init-сегмент. Вызывать ДО роликов ленты: он старше их, как на живой ленте. */
  static async open(context, catalog) {
    return new EarlierReel(context, await OffDocumentReel.open(catalog, 'earlier reel'));
  }

  /**
   * Качает больше, чем помнит мост: каждый `appendBuffer` — отдельная загрузка (свой буфер), как у настоящего
   * плеера сайта. В конце пишет в журнал `earlier-reel-downloaded mib=<N>`: по ней робот знает, что докачка была.
   */
  async downloadPastBridgeMemory() {
    const piece = await this.#reel.downloadPiece();
    let downloadedBytes = 0;
    while (downloadedBytes < DOWNLOAD_BYTES) {
      // `slice()` — новый буфер: мост считает память по буферам страницы, один и тот же он учёл бы один раз.
      await this.#reel.append(piece.slice(), 'earlier reel: downloading past the bridge memory');
      downloadedBytes += piece.length;
    }
    const { source, elementPaused } = this.#reel.state;
    this.#context.log(`earlier-reel-downloaded mib=${Math.round(downloadedBytes / MEBIBYTE)} source=${source} element_paused=${elementPaused}`);
  }
}

/** Толпа роликов вокруг ленты, как у живого Instagram: пройденные и дальние (см. шапку файла). */
export class ReelCrowd {
  #context;
  #catalog;
  #passedReels;
  /** Дальние ролики держим до конца страницы: закрытый MediaSource мост забыл бы, и строка мерила бы не то. */
  #farReels = [];

  constructor(context, catalog, passedReels) {
    this.#context = context;
    this.#catalog = catalog;
    this.#passedReels = passedReels;
  }

  /** Открывает пройденные ролики. Вызывать ДО роликов ленты: их MediaSource обязаны быть старше. */
  static async open(context, catalog) {
    const passedReels = [];
    for (let number = 1; number <= PASSED_REEL_COUNT; number += 1) {
      const reel = await OffDocumentReel.open(catalog, `passed reel ${number}`);
      const piece = await reel.downloadPiece();
      for (let downloaded = 0; downloaded < PASSED_REEL_BYTES; downloaded += piece.length) {
        await reel.append(piece.slice(), `passed reel ${number}: downloading`);
      }
      passedReels.push(reel);
    }
    return new ReelCrowd(context, catalog, passedReels);
  }

  /**
   * Страница сбрасывает пройденные ролики пачкой, открывает дальние и по кругу качает их начала сверх
   * потолка моста. В конце пишет `far-reels-preloaded passed_closed=<K> far_open=<M> mib=<N>`: по этой
   * отметке робот знает, что нагрузка была, а приговор видит, что пройденные закрыты, а дальние живы.
   */
  async detachPassedReelsAndPreloadFarAhead() {
    for (const reel of this.#passedReels) reel.detach();
    for (let number = 1; number <= FAR_REEL_COUNT; number += 1) {
      this.#farReels.push(await OffDocumentReel.open(this.#catalog, `far reel ${number}`));
    }
    const piece = await this.#farReels[0].downloadPiece();
    let downloadedBytes = 0;
    // По кругу, как живой сайт: по одному куску каждому ролику, потом следующий круг.
    for (let perReel = 0; perReel < FAR_REEL_BYTES; perReel += piece.length) {
      for (const [index, reel] of this.#farReels.entries()) {
        await reel.append(piece.slice(), `far reel ${index + 1}: preloading`);
        downloadedBytes += piece.length;
      }
    }
    const passedClosed = this.#passedReels.filter((reel) => reel.state.source === 'closed').length;
    const farOpen = this.#farReels.filter((reel) => reel.state.source === 'open').length;
    this.#context.log(`far-reels-preloaded passed_closed=${passedClosed} far_open=${farOpen} mib=${Math.round(downloadedBytes / MEBIBYTE)}`);
  }
}
