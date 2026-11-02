// Фейковый DOM для тестов тени: окно, события с фазами захвата/всплытия и границей shadow root,
// видеоэлемент с поведением «как у браузера» (доверенные события приходят микрозадачей).
//
// Каждый вызов `createFakeMediaDom()` создаёт НОВЫЕ классы: тень ставит переопределения на
// прототипы, и тесты не должны делить их между собой.

/** Запись о вызове оригинала настоящего элемента (для проверок «что тень сделала с элементом»). */
export type RealCall =
  | { readonly kind: "play" }
  | { readonly kind: "pause" }
  | { readonly kind: "load" }
  | { readonly kind: "set"; readonly property: string; readonly value: unknown };

interface ListenerEntry {
  readonly listener: (event: FakeEventShape) => void;
  readonly capture: boolean;
}

/** То, что тесты читают у события. */
export interface FakeEventShape {
  readonly type: string;
  readonly isTrusted: boolean;
  readonly target: unknown;
}

export interface FakeMediaState {
  currentTime: number;
  paused: boolean;
  playbackRate: number;
  defaultPlaybackRate: number;
  muted: boolean;
  volume: number;
  readyState: number;
  networkState: number;
  duration: number;
  src: string;
  srcObject: unknown;
  videoWidth: number;
  videoHeight: number;
  totalFrames: number;
  droppedFrames: number;
}

export function createFakeMediaDom() {
  class FakeEvent {
    readonly type: string;
    readonly bubbles: boolean;
    readonly composed: boolean;
    readonly isTrusted: boolean;
    #target: unknown = null;
    stoppedImmediately = false;
    stopped = false;
    constructor(type: string, init: { bubbles?: boolean; composed?: boolean } = {}) {
      this.type = type;
      this.bubbles = init.bubbles === true;
      this.composed = init.composed === true;
      // Как у браузера: `isTrusted` — собственное свойство экземпляра; созданные скриптом — false.
      this.isTrusted = false;
    }
    get target(): unknown {
      return this.#target;
    }
    setTargetForDispatch(target: unknown): void {
      this.#target = target;
    }
    stopImmediatePropagation(): void {
      this.stoppedImmediately = true;
      this.stopped = true;
    }
    stopPropagation(): void {
      this.stopped = true;
    }
  }

  /** Событие «от браузера» с `isTrusted=true` (ставит только фейковый «браузер»). */
  function trusted(type: string): FakeEvent {
    const event = new FakeEvent(type);
    Object.defineProperty(event, "isTrusted", { value: true });
    return event;
  }

  class FakeEventTarget {
    readonly listeners = new Map<string, ListenerEntry[]>();
    addEventListener(type: string, listener: (event: FakeEventShape) => void, options?: boolean | { capture?: boolean }): void {
      const capture = typeof options === "boolean" ? options : options?.capture === true;
      const entries = this.listeners.get(type) ?? [];
      if (entries.some((entry) => entry.listener === listener && entry.capture === capture)) return;
      entries.push({ listener, capture });
      this.listeners.set(type, entries);
    }
    removeEventListener(type: string, listener: (event: FakeEventShape) => void, options?: boolean | { capture?: boolean }): void {
      const capture = typeof options === "boolean" ? options : options?.capture === true;
      const entries = this.listeners.get(type) ?? [];
      this.listeners.set(
        type,
        entries.filter((entry) => !(entry.listener === listener && entry.capture === capture)),
      );
    }
    /** Следующий узел на пути события (или `undefined` — путь кончился). */
    eventParent(_event: FakeEvent): FakeEventTarget | undefined {
      return undefined;
    }
    dispatchEvent(event: FakeEvent): boolean {
      event.setTargetForDispatch(this);
      const path: FakeEventTarget[] = [this];
      for (let node = this.eventParent(event); node !== undefined; node = node.eventParent(event)) path.push(node);
      const invoke = (node: FakeEventTarget, phase: "capture" | "bubble" | "target"): void => {
        for (const entry of [...(node.listeners.get(event.type) ?? [])]) {
          if (event.stoppedImmediately) return;
          if (phase === "capture" && !entry.capture) continue;
          if (phase === "bubble" && entry.capture) continue;
          entry.listener(event);
        }
      };
      for (let index = path.length - 1; index >= 1; index -= 1) {
        if (event.stopped) return true;
        invoke(path[index] as FakeEventTarget, "capture");
      }
      if (event.stopped) return true;
      // У цели сначала слушатели захвата, затем остальные (как в современных Chromium/Firefox).
      for (const capturePass of [true, false]) {
        for (const entry of [...(this.listeners.get(event.type) ?? [])]) {
          if (event.stoppedImmediately) return true;
          if (entry.capture === capturePass) entry.listener(event);
        }
      }
      if (!event.bubbles) return true;
      for (let index = 1; index < path.length; index += 1) {
        if (event.stopped) return true;
        invoke(path[index] as FakeEventTarget, "bubble");
      }
      return true;
    }
  }

  class FakeNode extends FakeEventTarget {
    parentNode: FakeNode | undefined;
    getRootNode(): FakeNode {
      let node: FakeNode = this;
      while (node.parentNode !== undefined) node = node.parentNode;
      return node;
    }
    override eventParent(_event: FakeEvent): FakeEventTarget | undefined {
      return this.parentNode;
    }
    appendChild<Child extends FakeNode>(child: Child): Child {
      child.parentNode = this;
      return child;
    }
    /** Вынуть узел из дерева (как `Element.remove()`). */
    remove(): void {
      this.parentNode = undefined;
    }
    /** В документе ли узел (через хозяев shadow root — как в DOM). */
    get isConnected(): boolean {
      let node: FakeNode | undefined = this;
      while (node !== undefined) {
        if (node instanceof FakeDocument) return true;
        node = node instanceof FakeShadowRoot ? node.host : node.parentNode;
      }
      return false;
    }
  }

  class FakeDocument extends FakeNode {
    windowForEvents: FakeEventTarget | undefined;
    override eventParent(): FakeEventTarget | undefined {
      return this.windowForEvents;
    }
  }

  class FakeShadowRoot extends FakeNode {
    readonly host: FakeNode;
    constructor(host: FakeNode) {
      super();
      this.host = host;
    }
    // Не-`composed` событие не выходит из теневого дерева к хозяину.
    override eventParent(event: FakeEvent): FakeEventTarget | undefined {
      return event.composed ? this.host : undefined;
    }
  }

  class FakeElement extends FakeNode {}

  class FakeMediaSource {}
  class FakeMediaSourceHandle {}

  class FakeVideoPlaybackQuality {
    readonly #total: number;
    readonly #dropped: number;
    constructor(total: number, dropped: number) {
      this.#total = total;
      this.#dropped = dropped;
    }
    get totalVideoFrames(): number {
      return this.#total;
    }
    get droppedVideoFrames(): number {
      return this.#dropped;
    }
    get corruptedVideoFrames(): number {
      return 0;
    }
    get creationTime(): number {
      return 0;
    }
  }

  class FakeHTMLMediaElement extends FakeElement {
    readonly real: FakeMediaState = {
      currentTime: 0,
      paused: true,
      playbackRate: 1,
      defaultPlaybackRate: 1,
      muted: false,
      volume: 1,
      readyState: 4,
      networkState: 1,
      duration: 100,
      src: "blob:https://site.example/ms-1",
      srcObject: null,
      videoWidth: 1280,
      videoHeight: 720,
      totalFrames: 500,
      droppedFrames: 3,
    };
    readonly realCalls: RealCall[] = [];
    readonly frameCallbacks = new Map<number, unknown>();
    nextFrameHandle = 1;

    /** Доверенное событие «от браузера» приходит задачей позже (как настоящее). */
    fireTrusted(type: string): void {
      queueMicrotask(() => this.dispatchEvent(trusted(type)));
    }

    get currentTime(): number {
      return this.real.currentTime;
    }
    set currentTime(value: number) {
      this.realCalls.push({ kind: "set", property: "currentTime", value });
      this.real.currentTime = value;
      this.fireTrusted("seeking");
      this.fireTrusted("timeupdate");
      this.fireTrusted("seeked");
    }
    get paused(): boolean {
      return this.real.paused;
    }
    get ended(): boolean {
      return false;
    }
    get seeking(): boolean {
      return false;
    }
    get playbackRate(): number {
      return this.real.playbackRate;
    }
    set playbackRate(value: number) {
      const rate = Number(value);
      if (!Number.isFinite(rate)) throw new TypeError("non-finite rate");
      if (rate !== 0 && (rate < 0.0625 || rate > 16)) throw new DOMException("rate out of range", "NotSupportedError");
      this.realCalls.push({ kind: "set", property: "playbackRate", value: rate });
      this.real.playbackRate = rate;
      this.fireTrusted("ratechange");
    }
    get readyState(): number {
      return this.real.readyState;
    }
    get networkState(): number {
      return this.real.networkState;
    }
    get muted(): boolean {
      return this.real.muted;
    }
    set muted(value: boolean) {
      this.realCalls.push({ kind: "set", property: "muted", value });
      if (this.real.muted === Boolean(value)) return;
      this.real.muted = Boolean(value);
      this.fireTrusted("volumechange");
    }
    get volume(): number {
      return this.real.volume;
    }
    set volume(value: number) {
      const volume = Number(value);
      if (!(volume >= 0 && volume <= 1)) throw new DOMException("volume out of range", "IndexSizeError");
      this.realCalls.push({ kind: "set", property: "volume", value: volume });
      if (this.real.volume === volume) return;
      this.real.volume = volume;
      this.fireTrusted("volumechange");
    }
    get duration(): number {
      return this.real.duration;
    }
    get src(): string {
      return this.real.src;
    }
    set src(value: string) {
      this.real.src = String(value);
      this.#loadAlgorithm();
    }
    get srcObject(): unknown {
      return this.real.srcObject;
    }
    set srcObject(value: unknown) {
      this.real.srcObject = value;
      this.#loadAlgorithm();
    }
    play(): Promise<void> {
      this.realCalls.push({ kind: "play" });
      if (this.real.paused) {
        this.real.paused = false;
        this.fireTrusted("play");
        this.fireTrusted("playing");
      }
      return Promise.resolve();
    }
    pause(): void {
      this.realCalls.push({ kind: "pause" });
      if (this.real.paused) return;
      this.real.paused = true;
      this.fireTrusted("timeupdate");
      this.fireTrusted("pause");
    }
    fastSeek(seconds: number): void {
      this.currentTime = seconds;
    }
    load(): void {
      this.realCalls.push({ kind: "load" });
      this.#loadAlgorithm();
    }
    /** «Алгоритм загрузки» стандарта в той части, что видна тени. */
    #loadAlgorithm(): void {
      if (this.real.networkState !== 0) this.fireTrusted("emptied");
      this.real.paused = true;
      this.real.currentTime = 0;
      this.real.readyState = 0;
      this.real.playbackRate = this.real.defaultPlaybackRate;
      const hasSource = this.real.src !== "" || this.real.srcObject !== null;
      this.real.networkState = hasSource ? 2 : 0;
    }
  }

  class FakeHTMLVideoElement extends FakeHTMLMediaElement {
    get videoWidth(): number {
      return this.real.videoWidth;
    }
    get videoHeight(): number {
      return this.real.videoHeight;
    }
    get webkitDecodedFrameCount(): number {
      return this.real.totalFrames;
    }
    get webkitDroppedFrameCount(): number {
      return this.real.droppedFrames;
    }
    getVideoPlaybackQuality(): FakeVideoPlaybackQuality {
      return new FakeVideoPlaybackQuality(this.real.totalFrames, this.real.droppedFrames);
    }
    requestVideoFrameCallback(callback: unknown): number {
      const handle = this.nextFrameHandle;
      this.nextFrameHandle += 1;
      this.frameCallbacks.set(handle, callback);
      return handle;
    }
    cancelVideoFrameCallback(handle: number): void {
      this.frameCallbacks.delete(handle);
    }
  }

  const document = new FakeDocument();
  const body = document.appendChild(new FakeElement());

  class FakeWindow extends FakeEventTarget {
    readonly document = document;
    readonly HTMLMediaElement = FakeHTMLMediaElement;
    readonly HTMLVideoElement = FakeHTMLVideoElement;
    readonly EventTarget = FakeEventTarget;
    readonly Event = FakeEvent;
    readonly Node = FakeNode;
    readonly Promise = Promise;
    readonly DOMException = DOMException;
    readonly TypeError = TypeError;
    readonly MediaSource = FakeMediaSource;
    readonly MediaSourceHandle = FakeMediaSourceHandle;
    readonly VideoPlaybackQuality = FakeVideoPlaybackQuality;
    readonly navigator: { mediaSession?: unknown } = {};
    queueMicrotask(callback: () => void): void {
      if (this !== window) throw new TypeError("Illegal invocation");
      queueMicrotask(callback);
    }
  }
  const window = new FakeWindow();
  document.windowForEvents = window;

  /** Видеоэлемент в документе (или в переданном родителе). */
  function createVideo(parent: FakeNode = body): FakeHTMLVideoElement {
    return parent.appendChild(new FakeHTMLVideoElement());
  }

  /** Видеоэлемент внутри закрытого shadow root хозяина в документе. */
  function createVideoInShadowRoot(): { readonly video: FakeHTMLVideoElement; readonly shadowRoot: FakeShadowRoot } {
    const host = body.appendChild(new FakeElement());
    const shadowRoot = new FakeShadowRoot(host);
    return { video: createVideo(shadowRoot), shadowRoot };
  }

  return {
    window,
    document,
    body,
    trusted,
    createVideo,
    createVideoInShadowRoot,
    classes: { FakeHTMLMediaElement, FakeHTMLVideoElement, FakeEvent, FakeMediaSource, FakeVideoPlaybackQuality, FakeElement },
  };
}

export type FakeMediaDom = ReturnType<typeof createFakeMediaDom>;
export type FakeVideo = ReturnType<FakeMediaDom["createVideo"]>;

/** Дождаться всех микрозадач (доверенные события фейка и синтетические события тени). */
export function flushMicrotasks(): Promise<void> {
  return new Promise((resolve) => setImmediate(resolve));
}

/** Слушатель «как у страницы» на элементе: копит все события указанных типов. */
export function recordEvents(target: { addEventListener(type: string, listener: (event: FakeEventShape) => void): void }, types: readonly string[]) {
  const seen: { type: string; trusted: boolean }[] = [];
  for (const type of types) target.addEventListener(type, (event) => seen.push({ type: event.type, trusted: event.isTrusted }));
  return {
    seen,
    types: () => seen.map((entry) => entry.type),
    clear: () => {
      seen.length = 0;
    },
  };
}
