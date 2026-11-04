// Сборка записи в MAIN-мире страницы (отладочная сборка): рекордер, обёртки, подписчик на мост,
// приём пачек воркеров, выгрузка файла и отладочный глобал.
//
// Порядок (его держит `page/main.ts`):
//  1. `createPageRecording(win, …)` — на document_start ДО `startPageBridge`: даёт подписчика на
//     передачу и расширение каналов воркеров, которые мост принимает при сборке;
//  2. `startPageBridge(...)` — хуки моста и трекер;
//  3. `recording.install(bridge)` — обёртки записи ставятся ПОСЛЕ хука моста и теневых
//     переопределений, то есть снаружи них (см. `element-recording.ts`).
//
// Выгрузка: `__fastiplayerStreamRecorder.exportRecording()` (Promise) или событие окна
// `fastiplayer-stream-recorder-export`; сводка — `__fastiplayerStreamRecorder.summary()`.

import { captureBinaryIntrinsics, type BinaryIntrinsics, type BinaryWindow } from "../binary-view.ts";
import type { HandoffTap } from "../handoff-tap.ts";
import { captureMediaSourceReaders, type MediaSourceWindow } from "../media-source-readers.ts";
import type { MseTracker } from "../mse-tracker.ts";
import type { OddityReporter } from "../prototype-wrap.ts";
import type { WorkerChannel, WorkerChannelExtension } from "../worker-capture/index.ts";
import { installElementRecording, type ElementRecordingScope } from "./element-recording.ts";
import { installMediaSessionRecording, type MediaSessionRecordingScope } from "./media-session-recording.ts";
import { encodeRecordingParts, DEFAULT_STORE_CEILING_BYTES, type RecordedBrowser, type RecordingManifest } from "./format.ts";
import type { FileDownloader } from "./file-download.ts";
import { HandoffRecording } from "./handoff-recording.ts";
import { installMseRecording, type MseRecordingScope } from "./mse-recording.ts";
import { StreamRecorder, type RecordingSummary } from "./recorder.ts";
import { FLUSH_COMMAND_KIND, parseBatchReport } from "./worker-wire.ts";

/** Имя отладочного глобала и события выгрузки. */
export const RECORDER_GLOBAL_NAME = "__fastiplayerStreamRecorder";
export const RECORDER_EXPORT_EVENT = "fastiplayer-stream-recorder-export";

/** Сколько ждать ответ воркера на просьбу сдать пачку (мс), прежде чем выгрузить без него. */
const WORKER_FLUSH_TIMEOUT_MS = 1500;

/** Окно, против которого собирается запись (в тестах — фейк). */
export interface PageRecordingWindow extends MseRecordingScope, ElementRecordingScope, MediaSessionRecordingScope, BinaryWindow, MediaSourceWindow {
  readonly performance: { now(): number; readonly timeOrigin: number };
  readonly JSON: { stringify(value: unknown): string; parse(text: string): unknown };
  readonly setTimeout: (callback: () => void, delayMs: number) => unknown;
  readonly origin: string;
  addEventListener(type: string, listener: () => void): void;
}

export interface PageRecordingOptions {
  readonly browser: RecordedBrowser;
  readonly downloader: FileDownloader;
  readonly reportOddity: OddityReporter;
  /** Часы wall-clock (`Date.now`) — для `started_at_unix_ms`. */
  readonly wallClockMs: () => number;
  readonly storeCeilingBytes?: number;
}

/** Итог выгрузки (его видит скрипт, вызвавший её из отладочного протокола). */
export interface ExportResult {
  readonly filename: string;
  readonly size_bytes: number;
  readonly events: number;
  readonly truncated: boolean;
  readonly workers_flushed: number;
  readonly workers_timed_out: number;
}

export class PageRecording {
  readonly recorder: StreamRecorder;
  readonly tap: HandoffTap;
  readonly channelExtension: WorkerChannelExtension;
  readonly #win: PageRecordingWindow;
  readonly #options: PageRecordingOptions;
  readonly #binary: BinaryIntrinsics;
  readonly #startedAtUnixMs: number;
  readonly #channels: WorkerChannel[] = [];
  readonly #pendingFlushes = new Map<number, { remaining: Set<WorkerChannel>; done: () => void }>();
  #lastToken = 0;

  constructor(win: PageRecordingWindow, options: PageRecordingOptions) {
    this.#win = win;
    this.#options = options;
    this.#binary = captureBinaryIntrinsics(win);
    this.#startedAtUnixMs = options.wallClockMs();
    const now = win.performance.now.bind(win.performance);
    this.recorder = new StreamRecorder({
      ctx: "main",
      now,
      startMs: now(),
      storeCeilingBytes: options.storeCeilingBytes ?? DEFAULT_STORE_CEILING_BYTES,
    });
    const stringify = win.JSON.stringify.bind(win.JSON);
    const parse = win.JSON.parse.bind(win.JSON);
    this.tap = new HandoffRecording(this.recorder, (value) => parse(stringify(value)) as never);
    this.channelExtension = {
      channelCreated: (channel) => {
        this.#channels.push(channel);
      },
      acceptReport: (channel, data) => this.#acceptWorkerReport(channel, data, parse),
    };
  }

  /** Поставить обёртки записи (после `startPageBridge`). */
  install(tracker: MseTracker): void {
    const { reportOddity } = this.#options;
    const mediaSources = captureMediaSourceReaders(this.#win);
    installMseRecording(this.#win, {
      recorder: this.recorder,
      identity: { keyOf: (mediaSource) => tracker.recordOf(mediaSource)?.key ?? null, indexOf: (sourceBuffer) => tracker.sourceBufferIndexOf(sourceBuffer) },
      binary: this.#binary,
      isMediaSource: (candidate) => mediaSources.isMediaSource(candidate),
      reportOddity,
    });
    installElementRecording(this.#win, this.recorder, reportOddity);
    installMediaSessionRecording(this.#win, this.recorder, reportOddity);
    this.#installDebugSurface();
  }

  /** Сводка для скрипта: идёт ли запись и сколько в ней. */
  summary(): RecordingSummary & { readonly workers: number; readonly browser: RecordedBrowser; readonly origin: string } {
    return { ...this.recorder.summary(), workers: this.#channels.length, browser: this.#options.browser, origin: this.#win.origin };
  }

  /** Попросить воркеры сдать пачки, собрать файл и скачать его. */
  async exportRecording(): Promise<ExportResult> {
    const flush = await this.#flushWorkers();
    const manifest: RecordingManifest = {
      format: "fastiplayer-page-stream-recording",
      version: 1,
      browser: this.#options.browser,
      origin: this.#win.origin,
      started_at_unix_ms: this.#startedAtUnixMs,
      truncated: this.recorder.truncated,
      events: this.recorder.sortedEvents(),
    };
    const filename = `fastiplayer-${this.#win.origin.replace(/[^a-z0-9.-]+/gi, "_")}-${this.#startedAtUnixMs}.fprec`;
    const sizeBytes = this.#options.downloader.download(encodeRecordingParts(manifest, this.recorder.storeChunks), filename);
    return { filename, size_bytes: sizeBytes, events: manifest.events.length, truncated: manifest.truncated, ...flush };
  }

  // ------------------------------------------------------------ воркеры

  #acceptWorkerReport(channel: WorkerChannel, data: unknown, parseJson: (text: string) => unknown): boolean {
    const parsed = parseBatchReport(data, this.#binary, parseJson);
    if (parsed === undefined) return false;
    this.recorder.absorbWorkerBatch(channel.ordinal, parsed.batch, this.#win.performance.timeOrigin);
    if (parsed.token !== null) {
      const pending = this.#pendingFlushes.get(parsed.token);
      if (pending !== undefined) {
        pending.remaining.delete(channel);
        if (pending.remaining.size === 0) pending.done();
      }
    }
    return true;
  }

  /** Просьба всем воркерам; ждём ответы до `WORKER_FLUSH_TIMEOUT_MS`. */
  async #flushWorkers(): Promise<{ workers_flushed: number; workers_timed_out: number }> {
    this.#lastToken += 1;
    const token = this.#lastToken;
    const asked = new Set<WorkerChannel>();
    for (const channel of this.#channels) {
      try {
        channel.sendToExtension({ kind: FLUSH_COMMAND_KIND, token });
        asked.add(channel);
      } catch {
        // Канал без порта (воркер ещё не принял рукопожатие): ему сдавать нечего.
      }
    }
    if (asked.size === 0) return { workers_flushed: 0, workers_timed_out: 0 };
    const requested = asked.size;
    await new Promise<void>((resolve) => {
      this.#pendingFlushes.set(token, { remaining: asked, done: resolve });
      this.#win.setTimeout(resolve, WORKER_FLUSH_TIMEOUT_MS);
    });
    this.#pendingFlushes.delete(token);
    return { workers_flushed: requested - asked.size, workers_timed_out: asked.size };
  }

  // ------------------------------------------------------------ отладочная поверхность

  /** Глобал и событие окна — единственные места, где запись видна снаружи (только отладочная сборка). */
  #installDebugSurface(): void {
    const surface = {
      summary: () => this.summary(),
      exportRecording: () => this.exportRecording(),
    };
    Object.defineProperty(this.#win, RECORDER_GLOBAL_NAME, { value: surface, configurable: true, enumerable: false, writable: false });
    this.#win.addEventListener(RECORDER_EXPORT_EVENT, () => {
      this.exportRecording().catch((error: unknown) => this.#options.reportOddity(`export failed: ${String(error)}`));
    });
  }
}
