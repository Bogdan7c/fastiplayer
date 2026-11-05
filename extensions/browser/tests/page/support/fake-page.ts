// Фейковая страница для тестов MAIN-мира: MediaSource/SourceBuffer, URL, DOM с shadow root,
// окно с событиями и портами (порты — настоящие MessagePort из Node). Медиаэлемент умеет ровно
// столько, чтобы на нём жила НАСТОЯЩАЯ тень (`shadow-assembly.ts`): свойства, play/pause/load,
// атрибут `src` и «алгоритм загрузки», события через EventTarget Node.
//
// Каждый вызов `createFakePage()` создаёт СВОИ классы: хуки меняют прототипы, и тесты не должны
// делить подменённые прототипы между собой.

import type { HandoffEndPosition, HandoffEndResume, PageEvent, ShadowCommand } from "../../../src/shared/handoff-contract.ts";
import type { ShadowSessionHandle, ShadowSourceKind, ShadowStarter } from "../../../src/page/handoff-seam.ts";
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

/** Адрес документа фейка: геттер `src` отдаёт его для атрибута `src=""` (как браузер). */
export const FAKE_PAGE_URL = "https://site.example/watch";

/** `HTMLMediaElement.networkState` по стандарту. */
const NETWORK_STATE = { EMPTY: 0, IDLE: 1, LOADING: 2, NO_SOURCE: 3 } as const;

/** Настройки фейковой страницы. */
export interface FakePageOptions {
  /** У движка есть `Element.prototype.checkVisibility` (современный путь пробы видимости). */
  readonly nativeCheckVisibility?: boolean;
  /** Происхождение документа (`location.origin`); по умолчанию его нет. */
  readonly origin?: string;
}

/** Стиль элемента в фейке: что вернёт `getComputedStyle` (чего нет — значения по умолчанию CSS). */
export interface FakeStyle {
  display?: string;
  opacity?: string;
  visibility?: string;
}

/** Значения свойств по умолчанию, когда тест их не задал. */
const DEFAULT_STYLE: Required<FakeStyle> = { display: "block", opacity: "1", visibility: "visible" };

export function createFakePage(pageOptions: FakePageOptions = {}) {
  let urlCounter = 0;
  /** Сколько раз прочитан вычисленный стиль (проверка «опрос не квадратичен»). */
  const styleReads = { count: 0 };
  /** Слушатели фазы захвата: фейк не умеет всплытие, поэтому «путь события» повторяет по ним сам. */
  const captureListeners: { node: object; type: string; listener: unknown }[] = [];

  class FakeEventTarget extends EventTarget {
    override addEventListener(type: string, listener: EventListenerOrEventListenerObject | null, options?: boolean | AddEventListenerOptions): void {
      const capture = typeof options === "boolean" ? options : options?.capture === true;
      if (capture) captureListeners.push({ node: this, type, listener });
      super.addEventListener(type, listener, options);
    }
    // Тень берёт оригиналы методов прямо с `EventTarget.prototype`: у класса-обёртки они должны быть
    // собственными, а не унаследованными.
    override removeEventListener(type: string, listener: EventListenerOrEventListenerObject | null, options?: boolean | EventListenerOptions): void {
      super.removeEventListener(type, listener, options);
    }
    override dispatchEvent(event: Event): boolean {
      return super.dispatchEvent(event);
    }
  }
  /** `performance.now` страницы — под управлением теста (тень работает на тиках, не на таймерах). */
  const clock = {
    nowMs: 1_000,
    advance(ms: number): void {
      this.nowMs += ms;
    },
  };

  class FakeSourceBuffer extends EventTarget {
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

  class FakeMediaSource extends EventTarget {
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
      this.dispatchEvent(new Event("sourceended"));
    }
    /** Тестовый помощник: элемент подключил MediaSource (sourceopen). */
    testOpen(): void {
      this.#readyState = "open";
      this.dispatchEvent(new Event("sourceopen"));
    }
    /** Тестовый помощник: MediaSource отсоединён от элемента. */
    testClose(): void {
      this.#readyState = "closed";
      this.dispatchEvent(new Event("sourceclose"));
    }
  }

  /** Что стоит за каждым blob-адресом (обёртка воркера кладёт туда свои скрипты). */
  const objectUrlSubjects = new Map<string, unknown>();
  /** Отпущенные blob-адреса. */
  const revokedObjectUrls: string[] = [];

  // Разбор адресов — настоящий `URL` Node (обёртка воркера разрешает адрес скрипта от базы документа).
  class FakeURL extends URL {
    static override createObjectURL(subject: unknown): string {
      if (typeof subject !== "object" || subject === null) throw new TypeError("not a blob");
      urlCounter += 1;
      const url = `blob:https://site.example/${urlCounter}`;
      objectUrlSubjects.set(url, subject);
      return url;
    }
    static override revokeObjectURL(url: string): void {
      revokedObjectUrls.push(String(url));
    }
  }

  class FakeNode extends FakeEventTarget {
    connected = true;
    /** Родитель в дереве (плоский фейк: задаёт тест, чтобы проверять предков). */
    testParent: FakeNode | null = null;
    get parentNode(): FakeNode | null {
      return this.testParent;
    }
    get isConnected(): boolean {
      return this.connected;
    }
    /** Базовый адрес документа — адрес страницы. */
    get baseURI(): string {
      return FAKE_PAGE_URL;
    }
    /** Дерево у фейка плоское: корень любого узла — документ. */
    getRootNode(): unknown {
      return fakeDocument;
    }
  }

  class FakeDocumentFragment extends FakeNode {
    readonly children: FakeElement[] = [];
    /** Поддержаны селекторы «*» и имя тега; дети — плоский список (вложенность — через shadow root). */
    querySelectorAll(selector: string): FakeElement[] {
      return selector === "*" ? [...this.children] : this.children.filter((element) => element.localName === selector);
    }
  }

  class FakeShadowRoot extends FakeDocumentFragment {
    /** Хозяин shadow root (ставит `attachShadow`). */
    testHost: FakeNode | null = null;
    get host(): FakeNode | null {
      return this.testHost;
    }
  }

  class FakeElement extends FakeNode {
    readonly localName: string;
    rect = { left: 0, top: 0, right: 0, bottom: 0 };
    /** Стиль элемента (то, что отдаст `getComputedStyle`). */
    testStyle: FakeStyle = {};
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
      this.#shadowRoot.testHost = this;
      this.#shadowRoot.testParent = null;
      this.#shadowMode = init.mode;
      return this.#shadowRoot;
    }
    /** Что последним спросили у `checkVisibility` (путь «браузерная проба»). */
    lastCheckVisibilityOptions: unknown = null;
    getBoundingClientRect(): { left: number; top: number; right: number; bottom: number } {
      return { ...this.rect };
    }
    readonly #attributes = new Map<string, string>();
    getAttribute(name: string): string | null {
      return this.storedAttribute(String(name));
    }
    setAttribute(name: string, value: string): void {
      this.storeAttribute(String(name), String(value));
    }
    removeAttribute(name: string): void {
      this.storeAttribute(String(name), null);
    }
    protected storedAttribute(name: string): string | null {
      return this.#attributes.get(name) ?? null;
    }
    protected storeAttribute(name: string, value: string | null): void {
      if (value === null) this.#attributes.delete(name);
      else this.#attributes.set(name, value);
    }
  }

  class FakeMediaElement extends FakeElement {
    /** Подключённый MediaSource (держит его от сборщика мусора, как настоящий элемент). */
    testAttachedMediaSource: object | null = null;
    /** Атрибут `src` (`null` — атрибута нет). */
    #srcAttribute: string | null = null;
    #srcObject: unknown = null;
    #paused = true;
    #ended = false;
    #currentTime = 0;
    #playbackRate = 1;
    #muted = false;
    #volume = 1;
    #readyState = 0;
    #networkState: number = NETWORK_STATE.EMPTY;
    /** Как отражение URL-атрибута: нет атрибута — "", пустой атрибут — адрес документа. */
    get src(): string {
      if (this.#srcAttribute === null) return "";
      return this.#srcAttribute === "" ? FAKE_PAGE_URL : this.#srcAttribute;
    }
    set src(value: string) {
      this.storeAttribute("src", String(value));
    }
    get currentSrc(): string {
      return this.#srcAttribute ?? "";
    }
    get srcObject(): unknown {
      return this.#srcObject;
    }
    set srcObject(value: unknown) {
      this.#srcObject = value;
      this.#loadAlgorithm();
    }
    protected override storedAttribute(name: string): string | null {
      return name === "src" ? this.#srcAttribute : super.storedAttribute(name);
    }
    protected override storeAttribute(name: string, value: string | null): void {
      if (name !== "src") {
        super.storeAttribute(name, value);
        return;
      }
      this.#srcAttribute = value;
      // Стандарт: установка атрибута `src` запускает алгоритм загрузки, удаление — нет.
      if (value !== null) this.#loadAlgorithm();
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
    set currentTime(value: number) {
      this.#currentTime = Number(value);
    }
    get playbackRate(): number {
      return this.#playbackRate;
    }
    set playbackRate(value: number) {
      this.#playbackRate = Number(value);
    }
    get muted(): boolean {
      return this.#muted;
    }
    set muted(value: boolean) {
      this.#muted = Boolean(value);
    }
    get volume(): number {
      return this.#volume;
    }
    set volume(value: number) {
      this.#volume = Number(value);
    }
    get readyState(): number {
      return this.#readyState;
    }
    get networkState(): number {
      return this.#networkState;
    }
    /** Тестовая длительность настоящего элемента (`NaN` — метаданных нет). */
    testDuration = Number.NaN;
    get duration(): number {
      return this.testDuration;
    }
    #loop = false;
    get loop(): boolean {
      return this.#loop;
    }
    set loop(value: boolean) {
      this.#loop = Boolean(value);
    }
    /** Ключи, которые элемент держит (`null` — нет). */
    #mediaKeys: object | null = null;
    get mediaKeys(): object | null {
      return this.#mediaKeys;
    }
    /** Следующий `setMediaKeys` отклонит промис этой ошибкой. */
    testRejectNextSetMediaKeys: Error | null = null;
    /** Следующий `setMediaKeys` бросит синхронно (как при неверном аргументе). */
    testThrowNextSetMediaKeys: Error | null = null;
    /** Сколько раз браузерный `setMediaKeys` реально вызван (обёртка не должна терять вызовы). */
    setMediaKeysCalls = 0;
    /** Промис, который браузерный `setMediaKeys` отдал последним (страница обязана получить именно его). */
    lastSetMediaKeysPromise: Promise<void> | null = null;
    setMediaKeys(keys: object | null): Promise<void> {
      this.setMediaKeysCalls += 1;
      const outcome = this.#settleMediaKeys(keys);
      this.lastSetMediaKeysPromise = outcome;
      return outcome;
    }
    #settleMediaKeys(keys: object | null): Promise<void> {
      if (this.testThrowNextSetMediaKeys !== null) {
        const failure = this.testThrowNextSetMediaKeys;
        this.testThrowNextSetMediaKeys = null;
        throw failure;
      }
      if (this.testRejectNextSetMediaKeys !== null) {
        const failure = this.testRejectNextSetMediaKeys;
        this.testRejectNextSetMediaKeys = null;
        return Promise.reject(failure);
      }
      this.#mediaKeys = keys;
      return Promise.resolve();
    }
    /** Shadow root, в дереве которого лежит элемент (`null` — в основном документе). */
    testContainer: FakeShadowRoot | null = null;
    /**
     * Фейк не умеет всплытие, поэтому путь НЕ composed события `encrypted` повторяется вручную: после
     * обычной отправки его слышат слушатели фазы захвата окна (элемент в документе) или своего
     * shadow root (элемент в его дереве) — ровно как в браузере.
     */
    override dispatchEvent(event: Event): boolean {
      const delivered = super.dispatchEvent(event);
      if (event.type !== "encrypted") return delivered;
      const pathNode: object = this.testContainer ?? windowTarget;
      for (const entry of [...captureListeners]) {
        if (entry.node !== pathNode || entry.type !== event.type) continue;
        if (typeof entry.listener === "function") entry.listener.call(entry.node, event);
      }
      return delivered;
    }
    play(): Promise<void> {
      this.#paused = false;
      return Promise.resolve();
    }
    pause(): void {
      this.#paused = true;
    }
    load(): void {
      this.#loadAlgorithm();
    }
    /** «Алгоритм загрузки» стандарта в той части, что видна тени. */
    #loadAlgorithm(): void {
      if (this.#networkState !== NETWORK_STATE.EMPTY) queueMicrotask(() => this.dispatchEvent(trustedEvent("emptied")));
      this.#paused = true;
      this.#currentTime = 0;
      this.#readyState = 0;
      const hasSource = this.#srcObject !== null || (this.#srcAttribute !== null && this.#srcAttribute !== "");
      if (hasSource) this.#networkState = NETWORK_STATE.LOADING;
      else this.#networkState = this.#srcAttribute === "" ? NETWORK_STATE.NO_SOURCE : NETWORK_STATE.EMPTY;
    }
    /** Тестовый помощник: настоящее состояние элемента. */
    testSetState(state: { paused?: boolean; ended?: boolean; currentTime?: number; readyState?: number }): void {
      if (state.paused !== undefined) this.#paused = state.paused;
      if (state.ended !== undefined) this.#ended = state.ended;
      if (state.currentTime !== undefined) this.#currentTime = state.currentTime;
      if (state.readyState !== undefined) this.#readyState = state.readyState;
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
    get videoHeight(): number {
      return 360;
    }
  }

  /** Вычисленный стиль: читает `testStyle` элемента и считает чтения. */
  class FakeCssStyle {
    readonly #element: FakeElement;
    constructor(element: FakeElement) {
      // Как в браузере: стиль есть только у элемента, на документе `getComputedStyle` бросает.
      if (!(element instanceof FakeElement)) throw new TypeError("not an element");
      this.#element = element;
    }
    getPropertyValue(property: string): string {
      styleReads.count += 1;
      const style = this.#element.testStyle;
      return (style[property as keyof FakeStyle] ?? DEFAULT_STYLE[property as keyof FakeStyle]) as string;
    }
  }

  class FakeDocument extends FakeDocumentFragment {}

  /** Доверенное событие «от браузера» (`isTrusted` — собственное свойство, как у настоящего). */
  function trustedEvent(type: string): Event {
    const event = new Event(type);
    Object.defineProperty(event, "isTrusted", { value: true });
    return event;
  }

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
  const postedToPorts: { port: MessagePort; message: unknown; transfer: readonly Transferable[]; clonedBeforeSend: unknown }[] = [];
  const realPostMessage = MessagePort.prototype.postMessage;
  /**
   * Если задано — отправка в порт бросает (сбой копирования/порта в браузере); `only` сужает сбой
   * до сообщений, которые он принял (например, только команды каналу воркера).
   */
  const portFailure: { error: Error | null; only?: (message: unknown) => boolean } = { error: null };
  const spyPortPrototype = {
    postMessage(this: MessagePort, message: unknown, transfer: Transferable[]) {
      if (portFailure.error !== null && (portFailure.only?.(message) ?? true)) throw portFailure.error;
      // Клон до отправки: после transfer буферы сообщения у отправителя отсоединены.
      postedToPorts.push({ port: this, message, transfer, clonedBeforeSend: structuredClone(message) });
      Reflect.apply(realPostMessage, this, [message, transfer]);
    },
    start: MessagePort.prototype.start,
  };

  const windowTarget = new FakeEventTarget();
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
    location: { hostname: "site.example", ...(pageOptions.origin === undefined ? {} : { origin: pageOptions.origin }) },
    ShadowRoot: FakeShadowRoot,
    getComputedStyle: (element: FakeElement) => new FakeCssStyle(element),
    CSSStyleDeclaration: FakeCssStyle,
    EventTarget: FakeEventTarget,
    Event,
    Promise,
    DOMException,
    TypeError,
    queueMicrotask: (callback: () => void) => queueMicrotask(callback),
    // Таймер окна (страховка возврата файла тенью): тесты моста сроков не ждут.
    setTimeout: (callback: () => void, delayMs: number) => setTimeout(callback, delayMs),
    clearTimeout: (handle: ReturnType<typeof setTimeout>) => clearTimeout(handle),
    performance: { now: () => clock.nowMs },
    navigator: {},
    MessageEvent: WindowMessageEvent,
    MessagePort: { prototype: spyPortPrototype },
    Uint8Array,
    ArrayBuffer,
    DataView,
    crypto: globalThis.crypto,
  });

  if (pageOptions.nativeCheckVisibility === true) {
    // Браузер сам обходит предков: любой `display:none` / `opacity:0` в цепочке и `visibility` самого
    // элемента (так определяют спецификация и Chromium) — без чтений вычисленного стиля скриптом.
    Object.defineProperty(FakeElement.prototype, "checkVisibility", {
      configurable: true,
      value(this: FakeElement, options: unknown): boolean {
        this.lastCheckVisibilityOptions = options;
        let node: FakeNode | null = this;
        while (node !== null) {
          if (node instanceof FakeElement) {
            if ((node.testStyle.display ?? "block") === "none") return false;
            if (Number.parseFloat(node.testStyle.opacity ?? "1") === 0) return false;
          }
          node = node.testParent instanceof FakeShadowRoot ? node.testParent.testHost : node.testParent;
        }
        const visibility = this.testStyle.visibility ?? "visible";
        return visibility !== "hidden" && visibility !== "collapse";
      },
    });
  }

  return {
    win: win as unknown as PageBridgeWindow & EventTarget,
    styleReads,
    captureListeners,
    /** Доверенное событие «от браузера» (`isTrusted: true`). */
    browserEvent: trustedEvent,
    FakeShadowRoot,
    document: fakeDocument,
    postedToPorts,
    portFailure,
    clock,
    FakeMediaSource,
    FakeSourceBuffer,
    FakeURL,
    objectUrlSubjects,
    revokedObjectUrls,
    FakeElement,
    FakeVideoElement,
    WindowMessageEvent,
    /** Новый `<video>` в документе. */
    addVideo(): InstanceType<typeof FakeVideoElement> {
      const video = new FakeVideoElement();
      fakeDocument.children.push(video);
      return video;
    },
    /** Новый `<meta>` в документе, после всего, что там уже есть. */
    addMeta(name: string, content: string): void {
      const meta = new FakeElement("meta");
      meta.setAttribute("name", name);
      meta.setAttribute("content", content);
      fakeDocument.children.push(meta);
    },
    /** Новый `<video>` внутри shadow root (его дерево — отдельный путь событий). */
    addVideoInShadowRoot(root: InstanceType<typeof FakeShadowRoot>): InstanceType<typeof FakeVideoElement> {
      const video = new FakeVideoElement();
      video.testContainer = root;
      video.testParent = root;
      root.children.push(video);
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
  readonly started: {
    element: unknown;
    emit: (event: PageEvent) => void;
    isMediaSourceObjectUrl: (url: string) => boolean;
    source: ShadowSourceKind;
  }[] = [];
  readonly applied: ShadowCommand[] = [];
  readonly ended: { position: HandoffEndPosition; resume: HandoffEndResume }[] = [];
  /** Если задано — startShadow бросит (проверка «тень не встала»). */
  failToStart: Error | null = null;
  /** Слать ли handoff_ended из end() (настоящая тень шлёт). */
  emitsEndEvent = true;

  readonly starter: ShadowStarter = (element, emit, options) => {
    if (this.failToStart !== null) throw this.failToStart;
    this.started.push({ element, emit, isMediaSourceObjectUrl: options.isMediaSourceObjectUrl, source: options.source });
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
