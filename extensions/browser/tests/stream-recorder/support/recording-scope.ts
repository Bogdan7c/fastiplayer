// Стенд для тестов записи MSE: фейковое окно с полным набором операций MSE (EventTarget, как в
// браузере), НАСТОЯЩИЙ хук моста и трекер, поверх — НАСТОЯЩАЯ запись. Каждый вызов создаёт СВОИ
// классы: хуки меняют прототипы, и тесты не должны делить их между собой.

import { captureBinaryIntrinsics } from "../../../src/page/binary-view.ts";
import { DocumentMemoryBudget, DOCUMENT_MEMORY_BUDGET_BYTES } from "../../../src/page/journal/memory-budget.ts";
import { captureMediaSourceReaders } from "../../../src/page/media-source-readers.ts";
import { installMseHook } from "../../../src/page/mse-hook.ts";
import { MseTracker } from "../../../src/page/mse-tracker.ts";
import { installMseRecording } from "../../../src/page/stream-recorder/mse-recording.ts";
import { StreamRecorder, type RecorderOptions } from "../../../src/page/stream-recorder/recorder.ts";
import type { RecordedEvent } from "../../../src/page/stream-recorder/format.ts";

export const VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';

export function createRecordingScope() {
  class FakeSourceBuffer extends EventTarget {
    readonly appended: Uint8Array[] = [];
    failNextAppend: Error | null = null;
    modeValue = "segments";
    timestampOffsetValue = 0;
    appendWindowStartValue = 0;
    appendWindowEndValue = Infinity;
    bufferedRanges: [number, number][] = [];
    appendBuffer(source: ArrayBuffer | ArrayBufferView): void {
      if (this.failNextAppend !== null) {
        const failure = this.failNextAppend;
        this.failNextAppend = null;
        throw failure;
      }
      const view = ArrayBuffer.isView(source) ? new Uint8Array(source.buffer, source.byteOffset, source.byteLength) : new Uint8Array(source);
      this.appended.push(view.slice());
    }
    remove(_start: number, _end: number): void {}
    abort(): void {}
    changeType(_mime: string): void {}
    get mode(): string {
      return this.modeValue;
    }
    set mode(value: string) {
      this.modeValue = value;
    }
    get timestampOffset(): number {
      return this.timestampOffsetValue;
    }
    set timestampOffset(value: number) {
      this.timestampOffsetValue = Number(value);
    }
    get appendWindowStart(): number {
      return this.appendWindowStartValue;
    }
    set appendWindowStart(value: number) {
      this.appendWindowStartValue = Number(value);
    }
    get appendWindowEnd(): number {
      return this.appendWindowEndValue;
    }
    set appendWindowEnd(value: number) {
      this.appendWindowEndValue = Number(value);
    }
    get buffered(): { length: number; start(index: number): number; end(index: number): number } {
      const ranges = this.bufferedRanges;
      return { length: ranges.length, start: (index) => ranges[index]![0], end: (index) => ranges[index]![1] };
    }
  }

  class FakeMediaSource extends EventTarget {
    readyStateValue = "closed";
    durationValue = Number.NaN;
    readonly sourceBuffers: FakeSourceBuffer[] = [];
    get readyState(): string {
      return this.readyStateValue;
    }
    get duration(): number {
      return this.durationValue;
    }
    set duration(value: number) {
      this.durationValue = Number(value);
    }
    addSourceBuffer(_mime: string): FakeSourceBuffer {
      const sourceBuffer = new FakeSourceBuffer();
      this.sourceBuffers.push(sourceBuffer);
      return sourceBuffer;
    }
    removeSourceBuffer(sourceBuffer: FakeSourceBuffer): void {
      this.sourceBuffers.splice(this.sourceBuffers.indexOf(sourceBuffer), 1);
    }
    endOfStream(_error?: string): void {
      this.readyStateValue = "ended";
    }
    setLiveSeekableRange(_start: number, _end: number): void {}
    clearLiveSeekableRange(): void {}
    testOpen(): void {
      this.readyStateValue = "open";
      this.dispatchEvent(new Event("sourceopen"));
    }
    testEnded(): void {
      this.dispatchEvent(new Event("sourceended"));
    }
    testClose(): void {
      this.readyStateValue = "closed";
      this.dispatchEvent(new Event("sourceclose"));
    }
    static isTypeSupported(_mime: string): boolean {
      return true;
    }
  }

  let urlCounter = 0;
  class FakeURL {
    static createObjectURL(_subject: object): string {
      urlCounter += 1;
      return `blob:https://site.example/${urlCounter}`;
    }
  }

  /** Периодические задачи записи (`setInterval`): тест запускает их сам, без реального времени. */
  const intervals: (() => void)[] = [];
  const scope = {
    MediaSource: FakeMediaSource as { readonly prototype: object } & typeof FakeMediaSource,
    SourceBuffer: FakeSourceBuffer,
    URL: FakeURL,
    EventTarget,
    Uint8Array,
    ArrayBuffer,
    DataView,
    setInterval: (callback: () => void, _delayMs: number): number => intervals.push(callback),
  };
  return { scope, intervals, FakeMediaSource, FakeSourceBuffer, FakeURL };
}

export type RecordingScope = ReturnType<typeof createRecordingScope>;

/** Окружение записи: хук моста + запись, часы под управлением теста. */
export function startMseRecordingRig(overrides: Partial<RecorderOptions> = {}) {
  const rig = createRecordingScope();
  const oddities: string[] = [];
  const reportOddity = (what: string): void => {
    oddities.push(what);
  };
  const binary = captureBinaryIntrinsics(rig.scope);
  const mediaSources = captureMediaSourceReaders(rig.scope);
  const tracker = new MseTracker({
    budget: new DocumentMemoryBudget(DOCUMENT_MEMORY_BUDGET_BYTES),
    readyStateOf: (mediaSource) => mediaSources.readyStateOf(mediaSource),
    objectUrlOf: () => null,
    reportOddity,
  });
  installMseHook(rig.scope, { observer: tracker, binary, reportOddity });
  const clock = { nowMs: 100 };
  const recorder = new StreamRecorder({ ctx: "main", now: () => clock.nowMs, startMs: 100, ...overrides });
  installMseRecording(rig.scope, {
    recorder,
    identity: { keyOf: (mediaSource) => tracker.recordOf(mediaSource)?.key ?? null, indexOf: (sourceBuffer) => tracker.sourceBufferIndexOf(sourceBuffer) },
    binary,
    isMediaSource: (candidate) => mediaSources.isMediaSource(candidate),
    reportOddity,
  });
  return { ...rig, tracker, recorder, clock, oddities, binary };
}

/** События без времени — для сравнения с ожидаемым списком. */
export function withoutTime(events: readonly RecordedEvent[]): Record<string, unknown>[] {
  return events.map(({ t_ms: _time, ...rest }) => rest);
}

/** Склад целиком одним буфером. */
export function concatStore(chunks: readonly Uint8Array[]): Uint8Array {
  const total = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
  const joined = new Uint8Array(total);
  let position = 0;
  for (const chunk of chunks) {
    joined.set(chunk, position);
    position += chunk.length;
  }
  return joined;
}
