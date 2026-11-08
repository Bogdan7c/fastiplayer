// Фейковый DOM для слоя страницы экстеншена: узкий `UiDocument`, события, фейковая мышь и хранилище.
import type { PageUiRect } from "../../../src/shared/entry-contract.ts";
import type { PointerMoveEvent, PointerSource } from "../../../src/content/page-ui/overlay-button-controller.ts";
import type { UiDocument, UiElement, UiEvent, UiEventListener, UiHostElement, UiShadowRoot } from "../../../src/content/page-ui/ui-dom.ts";
import type { PlaceholderGeometryChange, PlaceholderGeometrySignals } from "../../../src/content/page-ui/placeholder-geometry.ts";

export class FakeUiEvent implements UiEvent {
  stopped = false;
  immediatelyStopped = false;
  prevented = false;
  readonly isTrusted: boolean;
  readonly target: unknown;
  constructor(isTrusted: boolean, target: unknown = null) {
    this.isTrusted = isTrusted;
    this.target = target;
  }
  stopPropagation(): void { this.stopped = true; }
  stopImmediatePropagation(): void { this.immediatelyStopped = true; }
  preventDefault(): void { this.prevented = true; }
}

export class FakeElement implements UiElement {
  readonly styleValues = new Map<string, { value: string; priority: string }>();
  readonly attributes = new Map<string, string>();
  readonly children: FakeElement[] = [];
  readonly listeners = new Map<string, Set<UiEventListener>>();
  parent: FakeElement | FakeShadow | FakeRoot | null = null;
  textContent: string | null = null;
  /** Прямоугольник, который «нарисовал бы браузер». */
  rect: PageUiRect = { left: 0, top: 0, width: 0, height: 0 };
  readonly style = {
    setProperty: (name: string, value: string, priority = ""): void => void this.styleValues.set(name, { value, priority }),
  };
  readonly tagName: string;
  constructor(tagName: string) {
    this.tagName = tagName;
  }
  get isConnected(): boolean {
    let node: FakeElement | FakeShadow | FakeRoot | null = this.parent;
    while (node !== null) {
      if (node instanceof FakeRoot) return true;
      node = node instanceof FakeShadow ? node.host : node.parent;
    }
    return false;
  }
  setAttribute(name: string, value: string): void { this.attributes.set(name, value); }
  append(...children: UiElement[]): void {
    for (const child of children as FakeElement[]) { child.parent = this; this.children.push(child); }
  }
  replaceChildren(...children: UiElement[]): void { this.children.length = 0; this.append(...children); }
  remove(): void {
    const siblings = this.parent === null ? null : this.parent instanceof FakeElement ? this.parent.children : this.parent.children;
    const at = siblings?.indexOf(this) ?? -1;
    if (at >= 0) siblings?.splice(at, 1);
    this.parent = null;
  }
  addEventListener(type: string, listener: UiEventListener): void {
    const set = this.listeners.get(type) ?? new Set();
    set.add(listener);
    this.listeners.set(type, set);
  }
  removeEventListener(type: string, listener: UiEventListener): void { this.listeners.get(type)?.delete(listener); }
  getBoundingClientRect(): PageUiRect { return this.rect; }
  /** Сколько слушателей всех видов висит на элементе. */
  listenerCount(): number { return [...this.listeners.values()].reduce((sum, set) => sum + set.size, 0); }
  /** Значение инлайнового свойства как строка «значение !priority». */
  css(name: string): string | undefined {
    const entry = this.styleValues.get(name);
    return entry === undefined ? undefined : entry.priority === "" ? entry.value : `${entry.value} !${entry.priority}`;
  }
  /** Доставить событие слушателям данного типа. */
  dispatch(type: string, event: UiEvent): void { for (const listener of [...(this.listeners.get(type) ?? [])]) listener(event); }
  descendants(): FakeElement[] { return this.children.flatMap((child) => [child, ...child.descendants()]); }
  findByText(text: string): FakeElement | undefined { return this.descendants().find((element) => element.textContent === text); }
}

export class FakeShadow implements UiShadowRoot {
  readonly children: FakeElement[] = [];
  readonly host: FakeHost;
  constructor(host: FakeHost) {
    this.host = host;
  }
  append(...children: UiElement[]): void {
    for (const child of children as FakeElement[]) { child.parent = this; this.children.push(child); }
  }
  all(): FakeElement[] { return this.children.flatMap((child) => [child, ...child.descendants()]); }
}

export class FakeHost extends FakeElement implements UiHostElement {
  shadow: FakeShadow | null = null;
  shadowMode: string | null = null;
  popoverOpen = false;
  popoverShows = 0;
  supportsPopover = true;
  showPopover?: () => void;
  hidePopover?: () => void;
  constructor(tagName: string, supportsPopover: boolean) {
    super(tagName);
    this.supportsPopover = supportsPopover;
    if (supportsPopover) {
      this.showPopover = () => { this.popoverOpen = true; this.popoverShows += 1; };
      this.hidePopover = () => { this.popoverOpen = false; };
    }
  }
  attachShadow(init: { readonly mode: "closed" }): UiShadowRoot {
    this.shadowMode = init.mode;
    this.shadow = new FakeShadow(this);
    return this.shadow;
  }
}

export class FakeRoot {
  readonly children: FakeElement[] = [];
  append(...children: UiElement[]): void {
    for (const child of children as FakeElement[]) { child.parent = this as unknown as FakeElement; this.children.push(child); }
  }
}

/** Фейковый документ; `ready=false` — корневого элемента ещё нет (document_start). */
export class FakeUiDocument implements UiDocument {
  root: FakeRoot | null = new FakeRoot();
  hosts: FakeHost[] = [];
  private readonly supportsPopover: boolean;
  constructor(supportsPopover = true) {
    this.supportsPopover = supportsPopover;
  }
  documentRoot(): FakeRoot | null { return this.root; }
  createHost(tagName: string): FakeHost { const host = new FakeHost(tagName, this.supportsPopover); this.hosts.push(host); return host; }
  createElement(tagName: string): FakeElement { return new FakeElement(tagName); }
  createPlayIcon(): FakeElement { return new FakeElement("svg"); }
  /** Все элементы тени единственного хоста. */
  shadowElements(): FakeElement[] { return this.hosts[0]?.shadow?.all() ?? []; }
}

/** Фейковая мышь документа: считает живые подписки. */
export class FakePointer implements PointerSource {
  readonly moveListeners = new Set<(event: PointerMoveEvent) => void>();
  readonly lostListeners = new Set<() => void>();
  onMouseMove(listener: (event: PointerMoveEvent) => void): () => void {
    this.moveListeners.add(listener);
    return () => void this.moveListeners.delete(listener);
  }
  onPointerLost(listener: () => void): () => void {
    this.lostListeners.add(listener);
    return () => void this.lostListeners.delete(listener);
  }
  liveListeners(): number { return this.moveListeners.size + this.lostListeners.size; }
  move(x: number, y: number, target: unknown = null): void { for (const listener of [...this.moveListeners]) listener({ x, y, target }); }
  lose(): void { for (const listener of [...this.lostListeners]) listener(); }
}

/** Фейковые сигналы «видео могло сдвинуться» для заглушки: считают подписки, тест шлёт сигналы сам. */
export class FakeGeometrySignals implements PlaceholderGeometrySignals {
  readonly watchers = new Map<object, Set<(change: PlaceholderGeometryChange) => void>>();
  watch(element: object, onChange: (change: PlaceholderGeometryChange) => void): () => void {
    const set = this.watchers.get(element) ?? new Set();
    set.add(onChange);
    this.watchers.set(element, set);
    return () => void set.delete(onChange);
  }
  /** Живые подписки на все элементы. */
  liveWatchers(): number { return [...this.watchers.values()].reduce((sum, set) => sum + set.size, 0); }
  emit(change: PlaceholderGeometryChange): void { for (const set of this.watchers.values()) for (const listener of [...set]) listener(change); }
}

/** Фейковый элемент страницы, над которым рисуется заглушка. */
export class FakeVideoTarget {
  isConnected = true;
  rect: PageUiRect;
  constructor(rect: PageUiRect) {
    this.rect = rect;
  }
  getBoundingClientRect(): PageUiRect { return this.rect; }
}

/** Ответ, который тест завершает сам, когда захочет (для гонки «мышь ушла раньше ответа»). */
export function deferred<Value>(): { promise: Promise<Value>; resolve: (value: Value) => void } {
  let resolve!: (value: Value) => void;
  const promise = new Promise<Value>((done) => { resolve = done; });
  return { promise, resolve };
}

/** Дать выполниться уже поставленным микрозадачам (цепочки then). */
export async function flushMicrotasks(): Promise<void> {
  for (let i = 0; i < 5; i++) await Promise.resolve();
}
