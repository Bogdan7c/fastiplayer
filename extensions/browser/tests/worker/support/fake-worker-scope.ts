// Фейковый глобал выделенного воркера для тестов хука (src/worker): MediaSource с `handle`,
// SourceBuffer, MediaSourceHandle, DedicatedWorkerGlobalScope с `postMessage`, события и порты —
// настоящие EventTarget/MessagePort Node.
//
// Каждый вызов создаёт СВОИ классы: хук меняет прототипы, тесты не должны делить подменённые.
// `postMessage` воркера ничего никуда не шлёт сам: всё уходит в `outbox` (и в `onPost`, если задан —
// так пара «воркер ⇄ страница» из tests/page/support/fake-worker-pair.ts доставляет сообщения).

/** Что воркер отправил через свой `postMessage` (снимок до отправки). */
export interface PostedFromWorker {
  readonly message: unknown;
  readonly transfer: readonly unknown[];
}

export function createFakeWorkerScope() {
  class FakeMediaSourceHandle {}
  /** Чей это handle (для пары «воркер ⇄ страница»: подключить MediaSource по handle главного потока). */
  const mediaSourceOfHandle = new WeakMap<object, FakeMediaSource>();

  class FakeSourceBuffer extends EventTarget {
    readonly appended: Uint8Array[] = [];
    appendBuffer(source: ArrayBuffer | ArrayBufferView): void {
      const view = ArrayBuffer.isView(source) ? new Uint8Array(source.buffer, source.byteOffset, source.byteLength) : new Uint8Array(source);
      this.appended.push(view.slice());
    }
    remove(start: number, end: number): void {
      if (!(Number(end) > Number(start))) throw new TypeError("bad range");
    }
    abort(): void {}
    changeType(): void {}
    get timestampOffset(): number {
      return 0;
    }
    set timestampOffset(_value: number) {}
  }

  class FakeMediaSource extends EventTarget {
    #readyState = "closed";
    #duration = Number.NaN;
    readonly #handle = new FakeMediaSourceHandle();
    readonly sourceBuffers: FakeSourceBuffer[] = [];
    constructor() {
      super();
      mediaSourceOfHandle.set(this.#handle, this);
    }
    get handle(): FakeMediaSourceHandle {
      return this.#handle;
    }
    get readyState(): string {
      return this.#readyState;
    }
    get duration(): number {
      return this.#duration;
    }
    set duration(value: number) {
      if (this.#readyState !== "open") throw new Error("InvalidStateError");
      this.#duration = value;
    }
    addSourceBuffer(_mimeType: string): FakeSourceBuffer {
      if (this.#readyState !== "open") throw new Error("InvalidStateError");
      const sourceBuffer = new FakeSourceBuffer();
      this.sourceBuffers.push(sourceBuffer);
      return sourceBuffer;
    }
    endOfStream(): void {
      this.#readyState = "ended";
    }
    static isTypeSupported(_mime: string): boolean {
      return true;
    }
    /** Тестовый помощник: элемент главного потока подключил handle (sourceopen). */
    testOpen(): void {
      this.#readyState = "open";
      this.dispatchEvent(new Event("sourceopen"));
    }
    /** Тестовый помощник: отсоединён от элемента (sourceclose). */
    testClose(): void {
      this.#readyState = "closed";
      this.dispatchEvent(new Event("sourceclose"));
    }
  }

  const outbox: PostedFromWorker[] = [];
  const delivery: { onPost: ((message: unknown, transfer: readonly unknown[]) => void) | null } = { onPost: null };

  class FakeDedicatedWorkerGlobalScope extends EventTarget {}

  // Как в Chromium (WebIDL [Global]): `postMessage` — собственное свойство САМОГО глобала, у
  // прототипа `DedicatedWorkerGlobalScope` его нет.
  function postMessage(message: unknown, transferOrOptions?: unknown): void {
    const transfer = Array.isArray(transferOrOptions)
      ? transferOrOptions
      : ((transferOrOptions as { transfer?: unknown[] } | undefined)?.transfer ?? []);
    outbox.push({ message, transfer });
    delivery.onPost?.(message, transfer);
  }

  const scope = Object.assign(new FakeDedicatedWorkerGlobalScope(), {
    postMessage,
    MediaSource: FakeMediaSource,
    SourceBuffer: FakeSourceBuffer,
    MediaSourceHandle: FakeMediaSourceHandle,
    DedicatedWorkerGlobalScope: FakeDedicatedWorkerGlobalScope,
    Uint8Array,
    ArrayBuffer,
    DataView,
    EventTarget,
    Event,
    MessageEvent,
    MessagePort,
  });

  /** Доверенное сообщение воркеру (как от браузера): данные и порты. */
  function deliverToWorker(data: unknown, ports: readonly MessagePort[] = []): void {
    const event = new MessageEvent("message", { data, ports: [...ports] });
    Object.defineProperty(event, "isTrusted", { value: true });
    scope.dispatchEvent(event);
  }

  return { scope, outbox, delivery, deliverToWorker, mediaSourceOfHandle, FakeMediaSource, FakeSourceBuffer, FakeMediaSourceHandle };
}

export type FakeWorkerScope = ReturnType<typeof createFakeWorkerScope>;
