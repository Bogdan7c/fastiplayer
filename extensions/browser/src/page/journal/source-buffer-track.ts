// Поток одного SourceBuffer внутри журнала MediaSource: смещения, границы сегментов, init-сегменты
// и то, что о нём помнит «снимок» выкинутого префикса.
//
// Владеет:
//  - сканером границ (формат — по MIME; после `changeType` — новый сканер);
//  - init-сегментами, собранными из видов на куски страницы (без копий; init может быть разрезан
//    на несколько кусков). Init-сегменты не выкидываются по потолку памяти: без последнего init
//    повтор не начать. Старые init отпускаются, когда их сменил более новый, уже лежащий в снимке;
//  - состоянием снимка: MIME, `timestampOffset` и выкинут ли сам `source_buffer_added`.
//
// Тонкость: заголовок бокса/элемента может начаться в одном куске, а дочитаться в следующих
// (YouTube режет где угодно, бывает и по байту). Тогда сканер сообщает границу со смещением,
// которое лежит в УЖЕ прошедшем куске («поздняя» граница). Поэтому дорожка помнит последние
// куски, покрывающие хотя бы длину самого длинного заголовка, — из них берётся начало init, а
// журнал по «поздней» границе дописывает точку разреза в прошлую запись.

import type { RetainedMemory } from "./retained-memory.ts";
import { isViewIntact, recordView, type RecordedView } from "./view-integrity.ts";
import { MAX_HEADER_BYTES } from "../segment-scanner/header-walker.ts";
import { createSegmentScanner, type SegmentScanner } from "../segment-scanner/scanner.ts";

/**
 * Предел одного init-сегмента. Настоящие init (ftyp+moov, EBML+Tracks) весят единицы-десятки КиБ;
 * 8 МиБ с запасом покрывают даже moov с длинными таблицами. Больше — значит, разбор принял за init
 * что-то другое, и держать это без границы памяти нельзя: журнал станет «неполным».
 */
export const MAX_INIT_SEGMENT_BYTES = 8 * 1024 * 1024;

/** Собранный init-сегмент: виды по порядку, склейка которых — ровно его байты. */
export interface InitSegment {
  /** Смещение начала в потоке SourceBuffer. */
  readonly start: number;
  readonly pieces: readonly RecordedView[];
}

/** Итог разбора куска для журнала. */
export type TrackScan =
  | {
      readonly kind: "scanned";
      readonly streamStart: number;
      /**
       * Смещения от начала потока, откуда его можно начать заново (по возрастанию). Смещение
       * меньше `streamStart` — «поздняя» граница: она в одном из прошлых кусков.
       */
      readonly cutOffsets: readonly number[];
    }
  | { readonly kind: "failed"; readonly reason: string };

/** Вид на кусок (с длинами при записи) вместе с его местом в потоке. */
interface PlacedChunk extends RecordedView {
  readonly start: number;
}

interface OpenInit {
  readonly start: number;
  readonly pieces: PlacedChunk[];
  /** До какого смещения потока init уже собран. */
  filledTo: number;
  bytes: number;
}

export class SourceBufferTrack {
  readonly index: number;
  readonly #memory: RetainedMemory;
  #scanner: SegmentScanner | null;
  /** Сколько байтов уже прошло через этот SourceBuffer (смещение следующего куска). */
  #streamOffset = 0;
  /**
   * Последние куски, вместе покрывающие ≥ `MAX_HEADER_BYTES` байт до текущего: в них могла
   * начаться «поздняя» граница. Это ссылки (не копии) на один-несколько последних кусков.
   */
  #recentChunks: PlacedChunk[] = [];
  #openInit: OpenInit | null = null;
  /** В этом разборе понадобился прошлый кусок с уже отсоединённым буфером. */
  #needsDetachedChunk = false;
  /** Законченные init-сегменты по порядку потока (обычно один; смена качества добавляет). */
  #inits: InitSegment[] = [];

  /** Снимок: `source_buffer_added` этого SourceBuffer уже выкинут из журнала. */
  snapshotAdded = false;
  /** Снимок: MIME на момент среза (после выкинутых `changeType`). */
  snapshotMime: string;
  /** Снимок: `timestampOffset` на момент среза, мкс. */
  snapshotOffsetUs = 0;
  /**
   * Выкинутый кусок кончился посреди сегмента, а следующего куска в журнале ещё нет: новый кусок
   * пригоден для повтора только с первой границы сегмента в нём.
   */
  resyncPending = false;

  constructor(index: number, mimeType: string, memory: RetainedMemory) {
    this.index = index;
    this.snapshotMime = mimeType;
    this.#memory = memory;
    this.#scanner = createSegmentScanner(mimeType, 0);
  }

  /** Понимает ли сканер формат этого SourceBuffer. */
  get isScannable(): boolean {
    return this.#scanner !== null;
  }

  /** `changeType`: дальше поток в новом формате; недособранный init забыт. */
  changeType(mimeType: string): void {
    this.#dropOpenInit();
    this.#recentChunks = [];
    this.#scanner = createSegmentScanner(mimeType, this.#streamOffset);
  }

  /** `abort()`: браузер сбросил разборщик, недособранный init выброшен. */
  restartAtNextAppend(): void {
    this.#dropOpenInit();
    this.#recentChunks = [];
    this.#scanner?.restartAtNextAppend();
  }

  /** Разобрать очередной кусок: границы и сборка init-сегментов (только виды, без копий). */
  scanChunk(view: Uint8Array): TrackScan {
    if (this.#scanner === null) return { kind: "failed", reason: "container is not scannable" };
    this.#needsDetachedChunk = false;
    const current: PlacedChunk = { ...recordView(view), start: this.#streamOffset };
    const chunkEnd = current.start + current.length;
    const outcome = this.#scanner.scan(view);
    this.#streamOffset = chunkEnd;
    if (outcome.kind === "failed") {
      this.#dropOpenInit();
      return outcome;
    }

    const sources = [...this.#recentChunks, current];
    const cutOffsets: number[] = [];
    for (const marker of outcome.markers) {
      switch (marker.kind) {
        case "init_start":
          this.#dropOpenInit(); // init без конца, за которым начался новый, — мусор
          this.#openInit = { start: marker.offset, pieces: [], filledTo: marker.offset, bytes: 0 };
          cutOffsets.push(marker.offset);
          break;
        case "init_end":
          this.#finishOpenInitAt(marker.offset, sources);
          break;
        case "media_start":
          // Медиасегмент без конца init (ftyp → moof без moov) — init не состоялся.
          this.#dropOpenInit();
          cutOffsets.push(marker.offset);
          break;
      }
    }
    this.#fillOpenInit(chunkEnd, sources);
    this.#rememberRecent(current);
    if (this.#needsDetachedChunk) {
      // Начало init лежит в прошлом куске, чей буфер страница уже отсоединила: собрать init
      // нельзя — честный сбой (журнал станет «неполным»), а не тихая порча.
      this.#dropOpenInit();
      return { kind: "failed", reason: "page buffer of a recent chunk was detached" };
    }
    if (this.#openInit !== null && this.#openInit.bytes > MAX_INIT_SEGMENT_BYTES) {
      this.#dropOpenInit();
      return { kind: "failed", reason: "init segment is larger than the limit" };
    }
    return { kind: "scanned", streamStart: current.start, cutOffsets };
  }

  /**
   * Выкинуто всё до смещения `evictedEnd`: из init-сегментов, начатых раньше, снимку нужен только
   * последний; более старые отпускаем.
   */
  releaseInitsSupersededBefore(evictedEnd: number): void {
    let lastBefore = -1;
    this.#inits.forEach((init, position) => {
      if (init.start < evictedEnd) lastBefore = position;
    });
    if (lastBefore <= 0) return;
    for (const superseded of this.#inits.slice(0, lastBefore)) this.#releasePieces(superseded.pieces);
    this.#inits = this.#inits.slice(lastBefore);
  }

  /**
   * Init-сегмент, который повтор должен отправить перед байтами, начинающимися со смещения
   * `replayStart` (`Infinity` — байтов в журнале нет). `null` — не нужен: повтор начинается с
   * самого init или init ещё не встречался.
   */
  initForReplayFrom(replayStart: number): InitSegment | null {
    if (this.#inits.some((init) => init.start === replayStart)) return null;
    let latest: InitSegment | null = null;
    for (const init of this.#inits) {
      if (init.start < replayStart) latest = init;
    }
    return latest;
  }

  /** Есть ли среди init-сегментов такой, чей буфер страница уже отсоединила или урезала. */
  hasBrokenInit(): boolean {
    return this.#inits.some((init) => !init.pieces.every(isViewIntact));
  }

  /** Отпустить испорченные init-сегменты (повтор их не отдаст: лучше без init, чем с мусором). */
  dropBrokenInits(): void {
    const broken = this.#inits.filter((init) => !init.pieces.every(isViewIntact));
    for (const init of broken) this.#releasePieces(init.pieces);
    this.#inits = this.#inits.filter((init) => !broken.includes(init));
  }

  /** Журнал стал «неполным»: оставить только последний законченный init, остальное отпустить. */
  keepOnlyLatestInit(): void {
    this.#dropOpenInit();
    this.#recentChunks = [];
    this.releaseInitsSupersededBefore(Number.POSITIVE_INFINITY);
  }

  /** Отпустить всё (MediaSource больше не нужен). */
  releaseEverything(): void {
    this.keepOnlyLatestInit();
    for (const init of this.#inits) this.#releasePieces(init.pieces);
    this.#inits = [];
  }

  /** Досборать открытый init до смещения `until` из известных кусков (виды, без копий). */
  #fillOpenInit(until: number, sources: readonly PlacedChunk[]): void {
    const openInit = this.#openInit;
    if (openInit === null) return;
    for (const source of sources) {
      const from = Math.max(openInit.filledTo, source.start);
      const to = Math.min(until, source.start + source.length);
      if (to <= from) continue;
      if (!isViewIntact(source)) {
        this.#needsDetachedChunk = true;
        return;
      }
      // subarray — новый вид на ту же память, не копия.
      const piece = source.view.subarray(from - source.start, to - source.start);
      openInit.pieces.push({ ...recordView(piece), start: from });
      openInit.bytes += piece.length;
      openInit.filledTo = to;
      this.#memory.retain(piece);
    }
  }

  /** Конец init на смещении `end` (может лежать и в прошлом куске — тогда лишний хвост срезается). */
  #finishOpenInitAt(end: number, sources: readonly PlacedChunk[]): void {
    const openInit = this.#openInit;
    if (openInit === null) return;
    if (end > openInit.filledTo) {
      this.#fillOpenInit(end, sources);
    } else {
      this.#trimOpenInitTo(openInit, end);
    }
    this.#inits.push({ start: openInit.start, pieces: openInit.pieces });
    this.#openInit = null;
  }

  /** Срезать собранные куски init так, чтобы он кончался ровно на `end`. */
  #trimOpenInitTo(openInit: OpenInit, end: number): void {
    while (openInit.pieces.length > 0) {
      const last = openInit.pieces[openInit.pieces.length - 1];
      if (last === undefined) break;
      const lastEnd = last.start + last.length;
      if (lastEnd <= end) break;
      openInit.pieces.pop();
      this.#memory.release(last.view);
      if (last.start < end) {
        if (!isViewIntact(last)) {
          this.#needsDetachedChunk = true;
          break;
        }
        const kept = last.view.subarray(0, end - last.start);
        openInit.pieces.push({ ...recordView(kept), start: last.start });
        this.#memory.retain(kept);
        break;
      }
    }
    openInit.filledTo = end;
  }

  /** Запомнить кусок и забыть те, что уже не могут содержать начало недочитанного заголовка. */
  #rememberRecent(chunk: PlacedChunk): void {
    this.#recentChunks.push(chunk);
    let newerBytes = 0;
    for (let position = this.#recentChunks.length - 1; position >= 0; position -= 1) {
      if (newerBytes >= MAX_HEADER_BYTES) {
        this.#recentChunks = this.#recentChunks.slice(position + 1);
        return;
      }
      newerBytes += this.#recentChunks[position]?.length ?? 0;
    }
  }

  #dropOpenInit(): void {
    if (this.#openInit === null) return;
    this.#releasePieces(this.#openInit.pieces);
    this.#openInit = null;
  }

  #releasePieces(pieces: readonly RecordedView[]): void {
    for (const piece of pieces) this.#memory.release(piece.view);
  }
}
