// Запись операций MSE внутри выделенного воркера (отладочная сборка): тот же рекордер и те же
// обёртки, что в главном потоке, но события копятся в воркере и уходят главному потоку пачками по
// личному порту канала (`worker-wire.ts`).
//
// Время событий — «сырое» `performance.now()` воркера; пересчёт на часы главного потока делает
// главный поток при приёме пачки (ему сообщается `performance.timeOrigin` воркера).

import type { ChunkTap } from "../page/handoff-tap.ts";
import { captureMediaSourceReaders } from "../page/media-source-readers.ts";
import { installMseRecording, type MseRecordingScope } from "../page/stream-recorder/mse-recording.ts";
import { StreamRecorder } from "../page/stream-recorder/recorder.ts";
import { BATCH_REPORT_KIND, parseFlushCommand, type BatchReport } from "../page/stream-recorder/worker-wire.ts";
import type { WorkerHookExtension, WorkerHookExtensionContext } from "./worker-hook.ts";

/** Глобал воркера с часами, JSON и таймером (в тестах — фейк). */
export interface WorkerRecordingScope extends MseRecordingScope {
  readonly performance: { now(): number; readonly timeOrigin: number };
  readonly JSON: { stringify(value: unknown): string };
}

/** Как часто воркер сдаёт пачку сам (мс), если главный поток не просил. */
const BATCH_INTERVAL_MS = 250;

/** Потолок склада воркера между отправками (мимо — `truncated`): отправки идут каждые 250 мс. */
const WORKER_STORE_CEILING_BYTES = 256 * 1024 * 1024;

/** Фабрика расширения хука воркера для записывающей сборки. */
export function createWorkerRecordingExtension(context: WorkerHookExtensionContext): WorkerHookExtension {
  const scope = context.scope as unknown as WorkerRecordingScope;
  const { link, tracker, binary, reportOddity } = context;
  const stringify = scope.JSON.stringify.bind(scope.JSON);
  const now = scope.performance.now.bind(scope.performance);
  const recorder = new StreamRecorder({ ctx: "worker", now, startMs: 0, storeCeilingBytes: WORKER_STORE_CEILING_BYTES });

  /** Отправить накопленное. Порта ещё нет — копим дальше (токен тогда остаётся без ответа). */
  const flush = (token: number | null): void => {
    if (!link.isConnected) return;
    if (token === null && recorder.eventCount === 0) return;
    const batch = recorder.drainBatch(scope.performance.timeOrigin);
    const report: BatchReport = {
      kind: BATCH_REPORT_KIND,
      token,
      time_origin: batch.timeOrigin,
      events_json: stringify(batch.events),
      bytes: batch.bytes,
      truncated: batch.truncated,
    };
    link.sendExtensionMessage(report, batch.bytes === null ? [] : [batch.bytes.buffer as ArrayBuffer]);
  };

  const chunkTap: ChunkTap = {
    chunkSent: (chunk) => {
      recorder.recordSent(chunk.mediaSource, chunk.sourceBuffer, chunk.bytes, chunk.origin);
    },
  };
  const mediaSources = captureMediaSourceReaders(scope as never);
  return {
    chunkTap,
    acceptCommand: (data) => {
      const command = parseFlushCommand(data);
      if (command === undefined) return false;
      flush(command.token);
      return true;
    },
    hookInstalled: () => {
      installMseRecording(scope, {
        recorder,
        identity: { keyOf: (mediaSource) => tracker.recordOf(mediaSource)?.key ?? null, indexOf: (sourceBuffer) => tracker.sourceBufferIndexOf(sourceBuffer) },
        binary,
        isMediaSource: (candidate) => mediaSources.isMediaSource(candidate),
        reportOddity,
      });
      // Плановая сдача пачек; таймер — оригинал, снятый до кода сайта (хук ставится раньше сайта).
      scope.setInterval?.(() => flush(null), BATCH_INTERVAL_MS);
    },
  };
}
