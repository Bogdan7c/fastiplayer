// Фейковая страница для тестов MAIN-мира: MediaSource/SourceBuffer, URL, DOM с shadow root,
// окно с событиями и портами (порты — настоящие MessagePort из Node).
//
// Каждый вызов `createFakePage()` создаёт СВОИ классы: хуки меняют прототипы, и тесты не должны
// делить подменённые прототипы между собой.

import type { HandoffEndPosition, HandoffEndResume, PageEvent, ShadowCommand } from "../../../src/shared/handoff-contract.ts";
import type { ShadowSessionHandle, ShadowStarter } from "../../../src/page/handoff-seam.ts";
import type { PageBridgeWindow } from "../../../src/page/bridge.ts";

/** Что сделал «браузер» на вызове appendBuffer (копия байтов, как в настоящем MSE). */
export interface AppendRecord {
  readonly bytes: Uint8Array;
}

/** Ошибка «браузера», как её бросил бы настоящий MSE. */
export class FakeDomException extends Error {
  constructor(name: string) {
    super(name);
    this.name = name;
  }
}

export function createFakePage() {
  let urlCounter = 0;

  class FakeSourceBuffer {
    readonly appended: AppendRecord[] = [];
    /** Следующий appendBuffer бросит эту ошибку (QuotaExceededError и т.п.). */
    failNextAppend: Error | null = null;
    #timestampOffset = 0;
    changedTypes: string[] = [];

    appendBuffer(source: ArrayBuffer | ArrayBufferView): void {
      if (this.failNextAppend !== null) {
        const failure = this.failNextAppend;
        this.failNextAppend = null;
        throw failure;
      }
      // Настоящий браузер копирует байты синхронно — фейк тоже.
      const view = ArrayBuffer.isView(source)
        ? new Uint8Array(source.buffer, source.byteOffset, source.byteLength)
        : new Uint8Array(source);
      this.appended.push({ bytes: view.slice() });
    }
    remove(start: number, end: number): void {
      if (!(Number(end) > Number(start))) throw new TypeError("bad range");
    }
    abort(): void {}
    changeType(mimeType: string): void {
      this.changedTypes.push(mimeType);
    }
    get timestampOffset(): number {
      return this.#timestampOffset;
    }
    set timestampOffset(input: number) {
      const value = Number(input); // WebIDL double: строка «3» → 3
      if (!Number.isFinite(value)) throw new TypeError("not finite");
      // Браузер нормализует значение — здесь округляем до миллисекунды, чтобы это было видно.
      this.#timestampOffset = Math.round(value * 1000) / 1000;
    }
  }

  class FakeMediaSource {
    #readyState = "closed";
    #duration = Number.NaN;
    readonly sourceBuffers: FakeSourceBuffer[] = [];

    get readyState(): string {
      return this.#readyState;
    }
    get duration(): number {
      return this.#duration;
    }
    set duration(value: number) {
      if (this.#readyState !== "open") throw new FakeDomException("InvalidStateError");
      if (Number.isNaN(value) || value < 0) throw new TypeError("bad duration");
      this.#duration = value;
    }
    addSourceBuffer(mimeType: string): FakeSourceBuffer {
      if (this.#readyState !== "open") throw new FakeDomException("InvalidStateError");
      if (String(mimeType).startsWith("unsupported/")) throw new FakeDomException("NotSupportedError");
      const sourceBuffer = new FakeSourceBuffer();
      this.sourceBuffers.push(sourceBuffer);
      return sourceBuffer;
    }
    /** Что браузер поставит в duration при endOfStream() без ошибки (конец буфера); `null` — не менять. */
    testDurationOnEnd: number | null = null;
    endOfStream(reason?: string): void {
      if (this.#readyState !== "open") throw new FakeDomException("InvalidStateError");
      this.#readyState = "ended";
      if (reason === undefined && this.testDurationOnEnd !== null) this.#duration = this.testDurationOnEnd;
    }
    /** Тестовый помощник: элемент подключил MediaSource (sourceopen). */
    testOpen(): void {
      this.#readyState = "open";
    }
    /** Тестовый помощник: MediaSource отсоединён от элемента. */
    testClose(): void {
      this.#readyState = "closed";
    }
  }

  class FakeURL {
    static createObjectURL(subject: unknown): string {
      if (typeof subject !== "object" || subject === null) throw new TypeError("not a blob");
      urlCounter += 1;
      return `blob:https://site.example/${urlCounter}`;
    }
  }

  class FakeNode {
    connected = true;
    get isConnected(): boolean {
      return this.connected;
    }
  }

  class FakeDocumentFragment extends FakeNode {
    readonly children: FakeElement[] = [];
    /** Поддержаны селекторы «*» и имя тега; дети — плоский список (вложенность — через shadow root). */
    querySelectorAll(selector: string): FakeElement[] {
      return selector === "*" ? [...this.children] : this.children.filter((element) => element.localName === selector);
    }
  }

  class FakeShadowRoot extends FakeDocumentFragment {}

  class FakeElement extends FakeNode {
    readonly localName: string;
    rect = { left: 0, top: 0, right: 0, bottom: 0 };
    #shadowRoot: FakeShadowRoot | null = null;
    #shadowMode: "open" | "closed" = "open";

    constructor(localName: string) {
      super();
      this.localName = localName;
    }
    get shadowRoot(): FakeShadowRoot | null {
      return this.#shadowMode === "open" ? this.#shadowRoot : null;
    }
    attachShadow(init: { mode: "open" | "closed" }): FakeShadowRoot {
      if (this.#shadowRoot !== null) throw new FakeDomException("NotSupportedError");
      this.#shadowRoot = new FakeShadowRoot();
      this.#shadowMode = init.mode;
      return this.#shadowRoot;
    }
    getBoundingClientRect(): { left: number; top: number; right: number; bottom: number } {
      return { ...this.rect };
    }
  }

  class FakeMediaElement extends FakeElement {
    /** Подключённый MediaSource (держит его от сборщика мусора, как настоящий элемент). */
    testAttachedMediaSource: object | null = null;
    #src = "";
    #paused = true;
    #ended = false;
    #currentTime = 0;
    get src(): string {
      return this.#src;
    }
    set src(value: string) {
      this.#src = value;
    }
    get currentSrc(): string {
      return this.#src;
    }
    get paused(): boolean {
      return this.#paused;
    }
    get ended(): boolean {
      return this.#ended;
    }
    get currentTime(): number {
      return this.#currentTime;
    }
    /** Тестовый помощник: настоящее состояние элемента. */
    testSetState(state: { paused?: boolean; ended?: boolean; currentTime?: number }): void {
      if (state.paused !== undefined) this.#paused = state.paused;
      if (state.ended !== undefined) this.#ended = state.ended;
      if (state.currentTime !== undefined) this.#currentTime = state.currentTime;
    }
  }

  class FakeVideoElement extends FakeMediaElement {
    readonly #width = 640;
    constructor() {
      super("video");
    }
    get videoWidth(): number {
      return this.#width;
    }
  }

  class FakeDocument extends FakeDocumentFragment {}

  /** У окна событий свой `source`: MessageEvent Node принимает источником только порт. */
  class WindowMessageEvent extends MessageEvent<unknown> {
    readonly #source: unknown;
    constructor(type: string, init: MessageEventInit<unknown>, source: unknown) {
      super(type, init);
      this.#source = source;
    }
    override get source(): MessageEventSource | null {
      return this.#source as MessageEventSource | null;
    }
  }

  /** Всё, что ушло в порты (снимается до отправки: виден список transfer). */
  const postedToPorts: { message: unknown; transfer: readonly Transferable[]; clonedBeforeSend: unknown }[] = [];
  const realPostMessage = MessagePort.prototype.postMessage;
  /** Если задано — отправка в порт бросает (сбой копирования/порта в браузере). */
  const portFailure: { error: Error | null } = { error: null };
  const spyPortPrototype = {
    postMessage(this: MessagePort, message: unknown, transfer: Transferable[]) {
      if (portFailure.error !== null) throw portFailure.error;
      // Клон до отправки: после transfer буферы сообщения у отправителя отсоединены.
      postedToPorts.push({ message, transfer, clonedBeforeSend: structuredClone(message) });
      Reflect.apply(realPostMessage, this, [message, transfer]);
    },
    start: MessagePort.prototype.start,
  };

  const windowTarget = new EventTarget();
  const fakeDocument = new FakeDocument();

  const win = Object.assign(windowTarget, {
    MediaSource: FakeMediaSource,
    SourceBuffer: FakeSourceBuffer,
    URL: FakeURL,
    document: fakeDocument,
    Document: FakeDocument,
    DocumentFragment: FakeDocumentFragment,
    Node: FakeNode,
    Element: FakeElement,
    HTMLMediaElement: FakeMediaElement,
    HTMLVideoElement: FakeVideoElement,
    innerWidth: 1280,
    innerHeight: 720,
    EventTarget,
    Event,
    MessageEvent: WindowMessageEvent,
    MessagePort: { prototype: spyPortPrototype },
    Uint8Array,
    ArrayBuffer,
    DataView,
    crypto: globalThis.crypto,
  });

  return {
    win: win as unknown as PageBridgeWindow & EventTarget,
    document: fakeDocument,
    postedToPorts,
    portFailure,
    FakeMediaSource,
    FakeSourceBuffer,
    FakeURL,
    FakeElement,
    FakeVideoElement,
    WindowMessageEvent,
    /** Новый `<video>` в документе. */
    addVideo(): InstanceType<typeof FakeVideoElement> {
      const video = new FakeVideoElement();
      fakeDocument.children.push(video);
      return video;
    },
    /**
     * Типичная страница: MediaSource → blob-адрес → src элемента → sourceopen. Вызовы идут через
     * прототипы (то есть через хук, если он стоит).
     */
    attachMediaSource(video: InstanceType<typeof FakeVideoElement>): InstanceType<typeof FakeMediaSource> {
      const mediaSource = new FakeMediaSource();
      video.src = FakeURL.createObjectURL(mediaSource);
      // Как в браузере: подключённый MediaSource жив, пока его держит элемент.
      video.testAttachedMediaSource = mediaSource;
      mediaSource.testOpen();
      return mediaSource;
    },
  };
}

export type FakePage = ReturnType<typeof createFakePage>;

/** Фейковая тень: записывает всё, что с ней делали; `end` шлёт handoff_ended, как настоящая. */
export class FakeShadow {
  readonly started: { element: unknown; emit: (event: PageEvent) => void; isMediaSourceObjectUrl: (url: string) => boolean }[] = [];
  readonly applied: ShadowCommand[] = [];
  readonly ended: { position: HandoffEndPosition; resume: HandoffEndResume }[] = [];
  /** Если задано — startShadow бросит (проверка «тень не встала»). */
  failToStart: Error | null = null;
  /** Слать ли handoff_ended из end() (настоящая тень шлёт). */
  emitsEndEvent = true;

  readonly starter: ShadowStarter = (element, emit, isMediaSourceObjectUrl) => {
    if (this.failToStart !== null) throw this.failToStart;
    this.started.push({ element, emit, isMediaSourceObjectUrl });
    const handle: ShadowSessionHandle = {
      apply: (command) => {
        this.applied.push(command);
      },
      end: (position, resume) => {
        this.ended.push({ position, resume });
        if (this.emitsEndEvent) emit({ kind: "handoff_ended", cause: "returned" });
      },
    };
    return handle;
  };
}
