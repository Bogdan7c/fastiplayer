// Журнал одного MediaSource: «память без копий» (решение 176).
//
// Зачем: посреди просмотра страница уже скачала 30–120 с вперёд и второй раз их не пришлёт. Чтобы
// плеер получил их при «отдать», MAIN-мир с самого начала держит ССЫЛКИ на куски `appendBuffer`
// (и события MSE) в порядке вызовов страницы. Пока передачи нет — ни одной копии.
//
// Устройство:
//  - записи по порядку: события MSE (`LocalMseEvent`) и виды на байты `appendBuffer`;
//  - потолок памяти общий на документ (`DocumentMemoryBudget`), он просит журнал выкинуть самую
//    старую запись (какой журнал документа отдаёт первым, решает потолок: memory-budget.ts).
//    Выкинутое не теряется бесследно, а сворачивается в «снимок»: MIME и `timestampOffset` каждого
//    SourceBuffer, длительность, последний init-сегмент (виды без копий);
//  - выкидывать байты можно только по границе сегмента: если выкинутый кусок кончился посреди
//    сегмента, следующие куски того же SourceBuffer до первой границы тоже бесполезны и
//    отпускаются, а повтор начнётся с границы (подмассив куска);
//  - формат, который сканер не понимает, или сбой разбора → журнал «неполный»: всё свёрнуто в
//    снимок, байты отпущены, дальше записи не копятся. Повтор такого журнала — только снимок;
//  - «уже просмотренное» (решение 364): потолок памяти сначала просит выкинуть то, что лежит позади
//    текущей секунды страницы. Журнал отвечает за свою половину: запись выкидывается, только если
//    после этого повтор её SourceBuffer начнётся с границы сегмента не позже названной секунды
//    (`evictOldestIfWatchedBefore`). Время границы читается лениво из заголовков fMP4;
//  - потолок числа записей `MAX_JOURNAL_ENTRIES`: память считает буферы, а не записи, и без него
//    события без байтов (сеттер `timestampOffset` в цикле) копились бы вечно, а у повтора не было
//    бы верхней границы (на неё опирается запас бюджета событий, content/page-event-budget.ts).
//
// Повтор (`replay`) = снимок (source_buffer_added, duration_changed, timestamp_offset_changed,
// init-байты) + оставшиеся записи по порядку. Журнал ведётся всегда, в том числе во время и после
// передачи: повторная передача после возврата должна работать.

import type { LocalMediaSourceKey, LocalMseEvent } from "../../shared/handoff-contract.ts";
import { MAX_JOURNAL_ENTRIES } from "../../shared/page-message-validation.ts";
import type { MediaDuration } from "../../protocol/time.ts";
import type { RetainedMemory } from "./retained-memory.ts";
import { SourceBufferTrack } from "./source-buffer-track.ts";
import { isViewIntact, recordView, type RecordedView } from "./view-integrity.ts";

/** Что повтор отдаёт наружу: события и виды на байты (копию делает отправитель). */
export type ReplayItem =
  | { readonly kind: "mse"; readonly event: LocalMseEvent }
  | { readonly kind: "bytes"; readonly sourceBuffer: number; readonly view: Uint8Array };

interface EventEntry {
  readonly kind: "mse";
  readonly event: LocalMseEvent;
}

interface BytesEntry {
  readonly kind: "bytes";
  readonly track: SourceBufferTrack;
  /**
   * Вид на байты страницы и его длины при записи. Смещения журнала считаются ТОЛЬКО по
   * запомненной длине: у отсоединённого страницей буфера вид пустеет (см. view-integrity.ts).
   */
  readonly recorded: RecordedView;
  /** Смещение первого байта куска в потоке SourceBuffer. */
  readonly streamStart: number;
  /** `timestampOffset` страницы в момент записи куска, мкс: сдвиг времени его сегментов на шкале страницы. */
  readonly pageOffsetUs: number;
  /**
   * Смещения внутри куска, с которых поток можно начать (границы сегментов, init), по возрастанию.
   * Может пополниться позже: заголовок, начатый в конце куска, дочитывается в следующем.
   */
  readonly cutPoints: number[];
  /** С какого байта куска повтор начинается (не 0 — начало куска выкинуто вместе с сегментом). */
  replayFrom: number;
  /** Кусок отпущен раньше своей очереди (кусок сегмента, начало которого выкинуто). */
  dead: boolean;
}

type JournalEntry = EventEntry | BytesEntry;

/** Состояние журнала. */
export type JournalCompleteness =
  /** Записи копятся, повтор = снимок + хвост. */
  | "complete"
  /** Формат не понят или разбор сбился: повтор = только снимок. */
  | "incomplete";

/** Когда стоит уплотнить массив записей (выкинутое начало — больше половины и не меньше этого). */
const COMPACT_AFTER_EVICTED_ENTRIES = 1024;

/** Микросекунд в секунде (времена договора — в микросекундах, позиция страницы — в секундах). */
const MICROS_PER_SECOND = 1_000_000;

/**
 * Что журнал помнит о самой старой записи с байтами: с какой секунды страницы начнётся повтор её
 * SourceBuffer, если её выкинуть. Потолок памяти спрашивает об одной и той же записи на каждый
 * `appendBuffer` страницы, пока зритель до неё не досмотрел: ответ считается один раз.
 */
interface ResumeTimeMemo {
  readonly entry: BytesEntry;
  /** `null` — неизвестно (границы ещё нет или её время не прочитано). */
  readonly resumeSeconds: number | null;
  /** Сколько записей было в журнале: «неизвестно» перепроверяется, когда страница дала новые куски. */
  readonly entryCount: number;
}

/** Окружение журнала: общая память документа. */
export interface JournalEnvironment {
  readonly memory: RetainedMemory;
}

export class MediaJournal {
  readonly key: LocalMediaSourceKey;
  readonly #environment: JournalEnvironment;
  readonly #tracks = new Map<number, SourceBufferTrack>();
  #entries: JournalEntry[] = [];
  /** Первая ещё не выкинутая запись. */
  #head = 0;
  #completeness: JournalCompleteness = "complete";
  /** Снимок: длительность на момент среза (`null` — не выставлялась). */
  #snapshotDuration: MediaDuration | null = null;
  /** Снимок: `endOfStream()` выкинут и после него ни байтов, ни `remove` не выкидывалось. */
  #snapshotEnded = false;
  #resumeTimeMemo: ResumeTimeMemo | null = null;
  #replayed = false;

  constructor(key: LocalMediaSourceKey, environment: JournalEnvironment) {
    this.key = key;
    this.#environment = environment;
  }

  get completeness(): JournalCompleteness {
    return this.#completeness;
  }

  /**
   * Повтор этого журнала уже запрашивали: видео его MediaSource уходило в плеер. Настоящий элемент при
   * этом стоит на паузе и «сыгранного» не копит, так что о просмотре в плеере знает только журнал.
   */
  get hasBeenReplayed(): boolean {
    return this.#replayed;
  }

  /** Записать событие MSE (уже успешный вызов страницы). */
  recordEvent(event: LocalMseEvent): void {
    this.#applyToTracks(event);
    if (this.#completeness === "incomplete") {
      this.#foldEvent(event);
      return;
    }
    this.#entries.push({ kind: "mse", event });
    this.#evictBeyondEntryCeiling();
    if (event.kind === "source_buffer_added" && !this.#tracks.get(event.source_buffer)?.isScannable) {
      this.markIncomplete();
    }
    if (event.kind === "source_buffer_type_changed" && !this.#tracks.get(event.source_buffer)?.isScannable) {
      this.markIncomplete();
    }
  }

  /** Записать ссылку на байты `appendBuffer` (без копии). Потолок памяти проверяет владелец бюджета. */
  recordBytes(sourceBuffer: number, view: Uint8Array): void {
    if (this.#completeness === "incomplete" || view.length === 0) return;
    const track = this.#tracks.get(sourceBuffer);
    if (track === undefined) return;
    const scan = track.scanChunk(view);
    if (scan.kind === "failed") {
      this.markIncomplete();
      return;
    }
    // Границы в этом куске (относительно его начала) и «поздние» — в прошлых кусках.
    const cutPoints: number[] = [];
    for (const offset of scan.cutOffsets) {
      if (offset >= scan.streamStart) cutPoints.push(offset - scan.streamStart);
      else this.#attachLateCut(track, offset);
    }
    let replayFrom = 0;
    if (track.resyncPending) {
      const firstCut = cutPoints[0];
      // Кусок — продолжение сегмента, начало которого выкинуто: без границы он бесполезен.
      if (firstCut === undefined) return;
      replayFrom = firstCut;
      track.resyncPending = false;
    }
    this.#entries.push({
      kind: "bytes",
      track,
      recorded: recordView(view),
      streamStart: scan.streamStart,
      pageOffsetUs: track.pageOffsetUs,
      cutPoints,
      replayFrom,
      dead: false,
    });
    this.#environment.memory.retain(view);
    this.#evictBeyondEntryCeiling();
  }

  /** Запись сверх потолка числа записей выкидывается так же, как при потолке памяти: старое — первым. */
  #evictBeyondEntryCeiling(): void {
    if (this.#entries.length - this.#head > MAX_JOURNAL_ENTRIES) this.evictOldest();
  }

  /** Есть ли у журнала запись, которую можно выкинуть. Init-сегмент выкидывание переживает: его держит дорожка. */
  hasEntriesToEvict(): boolean {
    this.#skipDeadHead();
    return this.#entries[this.#head] !== undefined;
  }

  /** Выкинуть самую старую запись, свернув её в снимок. */
  evictOldest(): void {
    this.#skipDeadHead();
    const entry = this.#entries[this.#head];
    if (entry === undefined) return;
    this.#head += 1;
    // Памятка держит запись (а та — вид на буфер страницы): выкинутую запись помнить нельзя.
    if (this.#resumeTimeMemo?.entry === entry) this.#resumeTimeMemo = null;
    if (entry.kind === "mse") {
      this.#foldEvent(entry.event);
    } else {
      this.#environment.memory.release(entry.recorded.view);
      this.#snapshotEnded = false;
      this.#realignAfterEvicted(entry);
    }
    this.#compactIfWorthIt();
  }

  /**
   * Выкинуть самую старую запись с байтами (и события перед ней), но только если зритель её уже
   * посмотрел: после выкидывания повтор её SourceBuffer начнётся с границы сегмента, которая на шкале
   * страницы стоит не позже `watchedUntilSeconds`. Тогда передача с текущей секунды страницы всё ещё
   * найдёт данные под собой.
   *
   * `false` — ничего не выкинуто: записей нет, следующей границы у этого SourceBuffer ещё нет (повтор
   * остался бы без начала) или её время неизвестно либо позже названной секунды.
   */
  evictOldestIfWatchedBefore(watchedUntilSeconds: number): boolean {
    const oldest = this.#oldestBytesEntry();
    if (oldest === null) return false;
    const resumeSeconds = this.#resumeSecondsAfterEvicting(oldest.entry, oldest.position);
    if (resumeSeconds === null || resumeSeconds > watchedUntilSeconds) return false;
    // События перед записью сворачиваются в снимок вместе с ней: порядок журнала не нарушается.
    for (;;) {
      this.#skipDeadHead();
      const head = this.#entries[this.#head];
      if (head === undefined) return true;
      this.evictOldest();
      if (head === oldest.entry) return true;
    }
  }

  /** Самая старая живая запись с байтами и её место в массиве записей. */
  #oldestBytesEntry(): { readonly entry: BytesEntry; readonly position: number } | null {
    for (let position = this.#head; position < this.#entries.length; position += 1) {
      const entry = this.#entries[position];
      if (entry !== undefined && entry.kind === "bytes" && !entry.dead) return { entry, position };
    }
    return null;
  }

  /**
   * С какой секунды страницы начнётся повтор SourceBuffer записи `evicted`, если её выкинуть: время
   * первой границы сегмента в следующих кусках того же SourceBuffer (тем же правилом, что
   * `#realignAfterEvicted`). `null` — границы ещё нет или её время неизвестно.
   */
  #resumeSecondsAfterEvicting(evicted: BytesEntry, evictedPosition: number): number | null {
    const memo = this.#resumeTimeMemo;
    if (memo !== null && memo.entry === evicted && (memo.resumeSeconds !== null || memo.entryCount === this.#entries.length)) {
      return memo.resumeSeconds;
    }
    const resumeSeconds = this.#readResumeSeconds(evicted, evictedPosition);
    this.#resumeTimeMemo = { entry: evicted, resumeSeconds, entryCount: this.#entries.length };
    return resumeSeconds;
  }

  #readResumeSeconds(evicted: BytesEntry, evictedPosition: number): number | null {
    const track = evicted.track;
    for (let position = evictedPosition + 1; position < this.#entries.length; position += 1) {
      const entry = this.#entries[position];
      if (entry === undefined || entry.kind !== "bytes" || entry.track !== track || entry.dead) continue;
      const boundary = entry.replayFrom > 0 ? entry.replayFrom : entry.cutPoints[0];
      // Кусок без границы — продолжение выкидываемого сегмента: он уйдёт вместе с ним.
      if (boundary === undefined) continue;
      const decodeSeconds = track.segmentStartSeconds(entry.streamStart + boundary, (offset, length) => this.#readTrackBytes(track, position, offset, length));
      return decodeSeconds === null ? null : decodeSeconds + entry.pageOffsetUs / MICROS_PER_SECOND;
    }
    return null;
  }

  /**
   * Байты потока SourceBuffer из записей журнала, начиная с записи `fromPosition` (копия нескольких
   * байт заголовка). `null` — этих байт в журнале нет или буфер страницы отсоединён.
   */
  #readTrackBytes(track: SourceBufferTrack, fromPosition: number, offset: number, length: number): Uint8Array | null {
    const bytes = new Uint8Array(length);
    let filled = 0;
    for (let position = fromPosition; position < this.#entries.length; position += 1) {
      const entry = this.#entries[position];
      if (entry === undefined || entry.kind !== "bytes" || entry.track !== track) continue;
      const from = offset + filled;
      const entryEnd = entry.streamStart + entry.recorded.length;
      if (from < entry.streamStart) return null; // нужное место раньше этой записи: его в журнале нет
      if (from >= entryEnd) continue;
      if (entry.dead || !isViewIntact(entry.recorded)) return null;
      const part = entry.recorded.view.subarray(from - entry.streamStart, Math.min(entryEnd, offset + length) - entry.streamStart);
      bytes.set(part, filled);
      filled += part.length;
      if (filled === length) return bytes;
    }
    return null;
  }

  /** Сделать журнал «неполным»: всё свернуть в снимок, байты отпустить. */
  markIncomplete(): void {
    if (this.#completeness === "incomplete") return;
    for (let position = this.#head; position < this.#entries.length; position += 1) {
      const entry = this.#entries[position];
      if (entry === undefined) continue;
      if (entry.kind === "mse") {
        this.#foldEvent(entry.event);
      } else if (!entry.dead) {
        this.#environment.memory.release(entry.recorded.view);
        this.#snapshotEnded = false;
      }
    }
    this.#entries = [];
    this.#head = 0;
    this.#resumeTimeMemo = null;
    this.#completeness = "incomplete";
    for (const track of this.#tracks.values()) track.keepOnlyLatestInit();
  }

  /** MediaSource больше не нужен (отсоединён/собран): отпустить всё, включая init. */
  discard(): void {
    this.markIncomplete();
    for (const track of this.#tracks.values()) track.releaseEverything();
  }

  /**
   * Повтор: снимок, затем оставшиеся записи по порядку. Только виды — без копий.
   * Если страница успела отсоединить буфер хоть одного нужного куска, журнал становится
   * «неполным» (повтор = снимок, дальше живое) — без тихой пропажи куска посреди потока.
   */
  replay(): ReplayItem[] {
    this.#replayed = true;
    this.#skipDeadHead();
    if (!this.#everyReplayedViewIntact()) {
      this.markIncomplete();
      for (const track of this.#tracks.values()) track.dropBrokenInits();
    }
    const items: ReplayItem[] = [];
    const snapshotTracks = [...this.#tracks.values()].filter((track) => track.snapshotAdded).sort((a, b) => a.index - b.index);
    for (const track of snapshotTracks) {
      items.push(this.#mse({ kind: "source_buffer_added", media_source: this.key, source_buffer: track.index, mime_type: track.snapshotMime }));
    }
    if (this.#snapshotDuration !== null && snapshotTracks.length > 0) {
      items.push(this.#mse({ kind: "duration_changed", media_source: this.key, duration: this.#snapshotDuration }));
    }
    for (const track of snapshotTracks) {
      if (track.snapshotOffsetUs !== 0) {
        items.push(
          this.#mse({ kind: "timestamp_offset_changed", media_source: this.key, source_buffer: track.index, offset_us: track.snapshotOffsetUs }),
        );
      }
      const init = track.initForReplayFrom(this.#firstReplayedOffset(track));
      for (const piece of init?.pieces ?? []) items.push({ kind: "bytes", sourceBuffer: track.index, view: piece.view });
    }
    if (this.#snapshotEnded) items.push(this.#mse({ kind: "end_of_stream", media_source: this.key }));
    for (let position = this.#head; position < this.#entries.length; position += 1) {
      const entry = this.#entries[position];
      if (entry === undefined) continue;
      if (entry.kind === "mse") {
        items.push(this.#mse(entry.event));
      } else if (!entry.dead) {
        const view = entry.replayFrom === 0 ? entry.recorded.view : entry.recorded.view.subarray(entry.replayFrom);
        items.push({ kind: "bytes", sourceBuffer: entry.track.index, view });
      }
    }
    return items;
  }

  #mse(event: LocalMseEvent): ReplayItem {
    return { kind: "mse", event };
  }

  /** События, меняющие поток SourceBuffer, сразу доходят до его дорожки (даже в «неполном»). */
  #applyToTracks(event: LocalMseEvent): void {
    switch (event.kind) {
      case "source_buffer_added":
        this.#tracks.set(event.source_buffer, new SourceBufferTrack(event.source_buffer, event.mime_type, this.#environment.memory));
        break;
      case "source_buffer_type_changed":
        this.#tracks.get(event.source_buffer)?.changeType(event.mime_type);
        break;
      case "source_buffer_aborted":
        this.#tracks.get(event.source_buffer)?.restartAtNextAppend();
        break;
      case "timestamp_offset_changed": {
        const track = this.#tracks.get(event.source_buffer);
        if (track !== undefined) track.pageOffsetUs = event.offset_us;
        break;
      }
      default:
        break;
    }
  }

  /** Свернуть выкинутое событие в снимок. */
  #foldEvent(event: LocalMseEvent): void {
    switch (event.kind) {
      case "source_buffer_added": {
        const track = this.#tracks.get(event.source_buffer);
        if (track === undefined) return;
        track.snapshotAdded = true;
        track.snapshotMime = event.mime_type;
        return;
      }
      case "source_buffer_type_changed": {
        const track = this.#tracks.get(event.source_buffer);
        if (track !== undefined) track.snapshotMime = event.mime_type;
        return;
      }
      case "timestamp_offset_changed": {
        const track = this.#tracks.get(event.source_buffer);
        if (track !== undefined) track.snapshotOffsetUs = event.offset_us;
        return;
      }
      case "duration_changed":
        this.#snapshotDuration = event.duration;
        return;
      case "end_of_stream":
        this.#snapshotEnded = true;
        return;
      case "source_buffer_removed":
        // remove() после endOfStream снова открывает MediaSource (спецификация MSE).
        this.#snapshotEnded = false;
        return;
      case "source_buffer_aborted":
        return;
    }
  }

  /**
   * Выкинут кусок `evicted`. Если он кончился посреди сегмента, следующий кусок того же
   * SourceBuffer надо начать с границы; куски без границы бесполезны и отпускаются сразу.
   */
  #realignAfterEvicted(evicted: BytesEntry): void {
    const track = evicted.track;
    const evictedEnd = evicted.streamStart + evicted.recorded.length;
    track.releaseInitsSupersededBefore(evictedEnd);
    for (let position = this.#head; position < this.#entries.length; position += 1) {
      const entry = this.#entries[position];
      if (entry === undefined || entry.kind !== "bytes" || entry.track !== track || entry.dead) continue;
      // Уже начинается с границы (кусок с неё начат или его начало уже срезано по границе).
      if (entry.replayFrom > 0 || entry.cutPoints[0] === 0) return;
      const firstCut = entry.cutPoints[0];
      if (firstCut !== undefined) {
        entry.replayFrom = firstCut;
        return;
      }
      entry.dead = true;
      this.#environment.memory.release(entry.recorded.view);
    }
    // Следующего куска ещё нет: им станет новый кусок страницы, он тоже начнётся с границы.
    track.resyncPending = true;
  }

  /**
   * Граница, найденная в уже записанном куске (её заголовок дочитан только сейчас): дописать точку
   * разреза в живую запись, которая содержит это смещение. Если запись уже выкинута или отпущена —
   * граница потеряна, повтор начнётся со следующей (это честнее, чем гадать).
   */
  #attachLateCut(track: SourceBufferTrack, offset: number): void {
    for (let position = this.#entries.length - 1; position >= this.#head; position -= 1) {
      const entry = this.#entries[position];
      if (entry === undefined || entry.kind !== "bytes" || entry.track !== track) continue;
      if (offset < entry.streamStart) continue; // граница в более раннем куске (куски бывают по байту)
      if (entry.dead) return;
      const relative = offset - entry.streamStart;
      if (relative < entry.recorded.length) entry.cutPoints.push(relative);
      return;
    }
  }

  /** Все виды, которые повтор отдаст (хвост и нужные init), всё ещё целы. */
  #everyReplayedViewIntact(): boolean {
    for (let position = this.#head; position < this.#entries.length; position += 1) {
      const entry = this.#entries[position];
      if (entry !== undefined && entry.kind === "bytes" && !entry.dead && !isViewIntact(entry.recorded)) return false;
    }
    return ![...this.#tracks.values()].some((track) => track.hasBrokenInit());
  }

  /** Смещение первого байта, который повтор отдаст из записей этого SourceBuffer. */
  #firstReplayedOffset(track: SourceBufferTrack): number {
    for (let position = this.#head; position < this.#entries.length; position += 1) {
      const entry = this.#entries[position];
      if (entry !== undefined && entry.kind === "bytes" && entry.track === track && !entry.dead) {
        return entry.streamStart + entry.replayFrom;
      }
    }
    return Number.POSITIVE_INFINITY;
  }

  #skipDeadHead(): void {
    while (true) {
      const entry = this.#entries[this.#head];
      if (entry === undefined || entry.kind !== "bytes" || !entry.dead) return;
      this.#head += 1;
    }
  }

  #compactIfWorthIt(): void {
    if (this.#head >= COMPACT_AFTER_EVICTED_ENTRIES && this.#head * 2 >= this.#entries.length) {
      this.#entries = this.#entries.slice(this.#head);
      this.#head = 0;
    }
  }
}
