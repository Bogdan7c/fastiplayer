// Чтение состояния DOM и видеоэлементов оригинальными геттерами, снятыми на document_start.
//
// Почему не `element.paused` напрямую: страница позже может подменить свойства прототипа (а
// «тень» сама переопределяет свойства переданного элемента, показывая странице время плеера).
// Нам же нужно состояние НАСТОЯЩЕГО элемента — его дают только геттеры, снятые до всех подмен.
// Все чтения — через `Reflect.apply`.

/** Прямоугольник элемента в CSS-пикселях окна. */
export interface ViewportRect {
  readonly left: number;
  readonly top: number;
  readonly right: number;
  readonly bottom: number;
}

/** Оригинальные чтения DOM, нужные MAIN-миру. Любое чтение может бросить (чужой «бренд»). */
export interface DomReaders {
  /** Все элементы документа (без содержимого shadow root). */
  elementsInDocument(): readonly Element[];
  /** Все элементы внутри shadow root (без вложенных shadow root). */
  elementsInShadowRoot(root: object): readonly Element[];
  /** Элемент — `<video>` (проверка «бренда» геттером HTMLVideoElement, без `instanceof`). */
  isVideoElement(candidate: unknown): boolean;
  /** Элемент или shadow root всё ещё в документе. */
  isConnected(node: object): boolean;
  /** Открытый shadow root элемента (`null` — нет или закрытый). */
  openShadowRootOf(host: object): object | null;
  /** Прямоугольник элемента относительно окна. */
  rectOf(element: object): ViewportRect;
  /** Размер окна в CSS-пикселях. */
  viewportSize(): { readonly width: number; readonly height: number };
  /** Атрибут-источник и выбранный источник настоящего элемента. */
  mediaSourceUrlsOf(element: object): readonly string[];
  /** Настоящий элемент на паузе / кончился / его настоящая позиция в секундах. */
  isPaused(element: object): boolean;
  isEnded(element: object): boolean;
  currentTimeSeconds(element: object): number;
  /** Выбранный источник настоящего элемента (`currentSrc`; "" — не выбран). */
  currentSrcOf(element: object): string;
  /** `srcObject` настоящего элемента (`null` — не задан или браузер его не знает). */
  srcObjectOf(element: object): unknown;
  /** Объект — MediaStream этого окна (камера, захват экрана, WebRTC). */
  isMediaStream(candidate: unknown): boolean;
  /** Объект — `Blob`/`File` этого окна. */
  isBlob(candidate: unknown): boolean;
  /**
   * Атрибут `type` того дочернего `<source>`, чей адрес совпадает с `url` (`null` — такого
   * `<source>` нет или у него нет `type`). По нему видно, что выбран HLS без «.m3u8» в адресе.
   */
  sourceTypeFor(element: object, url: string): string | null;
}

/** Окно с конструкторами DOM (в тестах — фейк). */
export interface DomWindow {
  readonly document: object;
  readonly Document: { readonly prototype: object };
  readonly DocumentFragment: { readonly prototype: object };
  readonly Node: { readonly prototype: object };
  readonly Element: { readonly prototype: object };
  readonly HTMLMediaElement: { readonly prototype: object };
  readonly HTMLVideoElement: { readonly prototype: object };
  /** Нет в старых/фейковых окнах — тогда тип `<source>` не читается. */
  readonly HTMLSourceElement?: { readonly prototype: object };
  readonly MediaStream?: { readonly prototype: object };
  readonly Blob?: { readonly prototype: object };
  readonly innerWidth: number;
  readonly innerHeight: number;
}

/**
 * Селектор «все элементы»: shadow root не видны снаружи, поэтому обход идёт по всем элементам
 * каждого корня в поисках хозяев открытых shadow root (и самих `<video>`). Это O(размер DOM), но
 * делается только по команде фона (выбор видео), а не постоянно.
 */
const ALL_ELEMENTS_SELECTOR = "*";

/** Найти аксессор по цепочке прототипов (геттер может жить у предка). */
function findGetter(prototype: object, name: string): (target: unknown) => unknown {
  let current: object | null = prototype;
  while (current !== null) {
    const getter = Object.getOwnPropertyDescriptor(current, name)?.get;
    if (getter !== undefined) return (target) => Reflect.apply(getter, target, []);
    current = Object.getPrototypeOf(current) as object | null;
  }
  return () => {
    throw new TypeError(`no getter ${name}`);
  };
}

/** Геттер, которого у браузера (или фейка) может не быть: `undefined` вместо бросающей заглушки. */
function findOptionalGetter(prototype: object | undefined, name: string): ((target: unknown) => unknown) | undefined {
  let current: object | null = prototype ?? null;
  while (current !== null) {
    const getter = Object.getOwnPropertyDescriptor(current, name)?.get;
    if (getter !== undefined) return (target) => Reflect.apply(getter, target, []);
    current = Object.getPrototypeOf(current) as object | null;
  }
  return undefined;
}

/** `prototype` в цепочке прототипов `candidate` (как `instanceof`, но без подменяемых глобалов). */
function hasPrototypeOf(prototype: object | undefined): (candidate: unknown) => boolean {
  const isPrototypeOf = Object.prototype.isPrototypeOf;
  return (candidate) =>
    prototype !== undefined && typeof candidate === "object" && candidate !== null && Reflect.apply(isPrototypeOf, prototype, [candidate]) === true;
}

/** Имя элемента `<source>` (`localName` у HTML-элементов — строчными). */
const SOURCE_ELEMENT_NAME = "source";

/** Найти метод по цепочке прототипов. */
function findMethod(prototype: object, name: string): (target: unknown, ...args: unknown[]) => unknown {
  let current: object | null = prototype;
  while (current !== null) {
    const method: unknown = Object.getOwnPropertyDescriptor(current, name)?.value;
    if (typeof method === "function") return (target, ...args) => Reflect.apply(method, target, args);
    current = Object.getPrototypeOf(current) as object | null;
  }
  return () => {
    throw new TypeError(`no method ${name}`);
  };
}

/** Список узлов (NodeList) → массив без чужих итераторов (индексы — внутренние). */
function toElementArray(list: unknown): Element[] {
  const indexed = list as { readonly length: number; readonly [index: number]: Element };
  const elements: Element[] = [];
  for (let index = 0; index < indexed.length; index += 1) {
    const element = indexed[index];
    if (element !== undefined) elements.push(element);
  }
  return elements;
}

/** Снять оригинальные чтения DOM. Зовётся один раз на document_start. */
export function captureDomReaders(win: DomWindow): DomReaders {
  const documentQuery = findMethod(win.Document.prototype, "querySelectorAll");
  const fragmentQuery = findMethod(win.DocumentFragment.prototype, "querySelectorAll");
  const isConnected = findGetter(win.Node.prototype, "isConnected");
  const shadowRoot = findGetter(win.Element.prototype, "shadowRoot");
  const boundingRect = findMethod(win.Element.prototype, "getBoundingClientRect");
  // `videoWidth` есть только у HTMLVideoElement: геттер бросит на любом другом объекте.
  const videoWidth = findGetter(win.HTMLVideoElement.prototype, "videoWidth");
  const mediaPrototype = win.HTMLMediaElement.prototype;
  const src = findGetter(mediaPrototype, "src");
  const currentSrc = findGetter(mediaPrototype, "currentSrc");
  const paused = findGetter(mediaPrototype, "paused");
  const ended = findGetter(mediaPrototype, "ended");
  const currentTime = findGetter(mediaPrototype, "currentTime");
  const srcObject = findOptionalGetter(mediaPrototype, "srcObject");
  const children = findOptionalGetter(win.Element.prototype, "children");
  const localName = findOptionalGetter(win.Element.prototype, "localName");
  const getAttribute = findMethod(win.Element.prototype, "getAttribute");
  const sourceSrc = findOptionalGetter(win.HTMLSourceElement?.prototype, "src");
  const isMediaStream = hasPrototypeOf(win.MediaStream?.prototype);
  const isBlob = hasPrototypeOf(win.Blob?.prototype);
  const documentObject = win.document;

  return {
    elementsInDocument: () => toElementArray(documentQuery(documentObject, ALL_ELEMENTS_SELECTOR)),
    elementsInShadowRoot: (root) => toElementArray(fragmentQuery(root, ALL_ELEMENTS_SELECTOR)),
    isVideoElement: (candidate) => {
      try {
        videoWidth(candidate);
        return true;
      } catch {
        return false;
      }
    },
    isConnected: (node) => isConnected(node) === true,
    openShadowRootOf: (host) => {
      const root = shadowRoot(host);
      return typeof root === "object" && root !== null ? root : null;
    },
    rectOf: (element) => {
      const rect = boundingRect(element) as ViewportRect;
      return { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom };
    },
    viewportSize: () => ({ width: win.innerWidth, height: win.innerHeight }),
    mediaSourceUrlsOf: (element) => {
      const urls = [src(element), currentSrc(element)];
      return urls.filter((url): url is string => typeof url === "string" && url.length > 0);
    },
    isPaused: (element) => paused(element) === true,
    isEnded: (element) => ended(element) === true,
    currentTimeSeconds: (element) => {
      const seconds = currentTime(element);
      return typeof seconds === "number" ? seconds : Number.NaN;
    },
    currentSrcOf: (element) => {
      const url = currentSrc(element);
      return typeof url === "string" ? url : "";
    },
    srcObjectOf: (element) => (srcObject === undefined ? null : (srcObject(element) ?? null)),
    isMediaStream,
    isBlob,
    sourceTypeFor: (element, url) => {
      if (children === undefined || localName === undefined || sourceSrc === undefined) return null;
      for (const child of toElementArray(children(element))) {
        if (localName(child) !== SOURCE_ELEMENT_NAME || sourceSrc(child) !== url) continue;
        const type = getAttribute(child, "type");
        return typeof type === "string" ? type : null;
      }
      return null;
    },
  };
}
