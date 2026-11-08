// Чтение состояния DOM и видеоэлементов оригинальными геттерами, снятыми на document_start.
//
// Почему не `element.paused` напрямую: страница позже может подменить свойства прототипа (а
// «тень» сама переопределяет свойства переданного элемента, показывая странице время плеера).
// Нам же нужно состояние НАСТОЯЩЕГО элемента — его дают только геттеры, снятые до всех подмен.
// Все чтения — через `Reflect.apply`.

import { captureVisibilityReaders, type VisibilityProbe, type VisibilityWindow } from "./visibility-probe.ts";

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
  /** `readyState` настоящего элемента (0 — метаданных ещё нет). */
  readyStateOf(element: object): number;
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
  /** `duration` настоящего элемента, секунды (`NaN` — метаданных нет, `Infinity` — эфир). */
  durationSecondsOf(element: object): number;
  /** Настоящий элемент без звука: `muted` ИЛИ громкость 0. */
  isSilent(element: object): boolean;
  /** `loop` настоящего элемента. */
  isLooping(element: object): boolean;
  /** `videoWidth` и `videoHeight` настоящего элемента (0 — размер кадра ещё неизвестен). */
  videoSizeOf(element: object): { readonly width: number; readonly height: number };
  /**
   * У настоящего элемента ошибка ВОСПРОИЗВЕДЕНИЯ (сессия 11a): `error.code` — `MEDIA_ERR_NETWORK` или
   * `MEDIA_ERR_DECODE`. По ней сторож видит сломавшийся плеер страницы. `MEDIA_ERR_SRC_NOT_SUPPORTED` сюда
   * не входит: его браузер ставит и тогда, когда страница сама убирает источник (`src = ""`), а это честное
   * «страница забрала видео» (решение 179), а не поломка. `false` и там, где геттера нет.
   */
  hasPlaybackFailureError(element: object): boolean;
  /** У элемента есть ключи (`mediaKeys !== null`); `false` и там, где геттера нет (не защищённый контекст). */
  hasMediaKeys(element: object): boolean;
  /** Имя хоста документа (`location.hostname`; "" — нет). */
  documentHostName(): string;
  /** Происхождение документа, снятое на старте; `null` — непрозрачное или неизвестно. */
  documentOrigin(): string | null;
  /** Атрибут `referrerpolicy` элемента как есть; `null` — атрибута нет. */
  referrerPolicyOf(element: object): string | null;
  /** Значения `content` у `<meta name=referrer>` в порядке документа: последнее годное и действует. */
  metaReferrerContents(): readonly string[];
  /** Новая проба скрытости на один опрос документа (кэш предков живёт ровно столько). */
  createVisibilityProbe(): VisibilityProbe;
}

/** Окно с конструкторами DOM (в тестах — фейк). */
export interface DomWindow extends VisibilityWindow {
  readonly document: object;
  readonly Document: { readonly prototype: object };
  readonly DocumentFragment: { readonly prototype: object };
  readonly Node: { readonly prototype: object };
  readonly Element: { readonly prototype: object };
  readonly HTMLMediaElement: { readonly prototype: object };
  readonly HTMLVideoElement: { readonly prototype: object };
  /** Нет в старых/фейковых окнах — тогда тип `<source>` не читается. */
  readonly HTMLSourceElement?: { readonly prototype: object };
  /** Нет в фейковых окнах — тогда код ошибки читается обычным свойством. */
  readonly MediaError?: { readonly prototype: object };
  readonly MediaStream?: { readonly prototype: object };
  readonly Blob?: { readonly prototype: object };
  readonly innerWidth: number;
  readonly innerHeight: number;
  /** Адрес документа; нет — имя хоста пустое, а происхождение неизвестно. */
  readonly location?: { readonly hostname: string; readonly origin?: string };
}

/**
 * Селектор «все элементы»: shadow root не видны снаружи, поэтому обход идёт по всем элементам
 * каждого корня в поисках хозяев открытых shadow root (и самих `<video>`). Это O(размер DOM), но
 * делается только по команде фона (выбор видео), а не постоянно.
 */
const ALL_ELEMENTS_SELECTOR = "*";

/** Селектор `<meta>`: имя сверяется уже оригинальным `getAttribute`, без учёта регистра. */
const META_SELECTOR = "meta";

/** Значение `name` у `<meta>`, задающего политику Referer документа. */
const REFERRER_META_NAME = "referrer";

/** Сериализация непрозрачного происхождения (`about:blank`, `data:`). */
const OPAQUE_ORIGIN = "null";

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

/** `MediaError.MEDIA_ERR_NETWORK`: сеть оборвала уже доступный ресурс (для MSE — `endOfStream("network")`). */
const MEDIA_ERR_NETWORK = 2;
/** `MediaError.MEDIA_ERR_DECODE`: браузер не смог декодировать куски (битые байты, `endOfStream("decode")`). */
const MEDIA_ERR_DECODE = 3;

/** Снять оригинальные чтения DOM. Зовётся один раз на document_start. */
export function captureDomReaders(win: DomWindow): DomReaders {
  const documentQuery = findMethod(win.Document.prototype, "querySelectorAll");
  const fragmentQuery = findMethod(win.DocumentFragment.prototype, "querySelectorAll");
  const isConnected = findGetter(win.Node.prototype, "isConnected");
  const shadowRoot = findGetter(win.Element.prototype, "shadowRoot");
  const boundingRect = findMethod(win.Element.prototype, "getBoundingClientRect");
  // `videoWidth` есть только у HTMLVideoElement: геттер бросит на любом другом объекте.
  const videoWidth = findGetter(win.HTMLVideoElement.prototype, "videoWidth");
  const videoHeight = findGetter(win.HTMLVideoElement.prototype, "videoHeight");
  const mediaPrototype = win.HTMLMediaElement.prototype;
  const src = findGetter(mediaPrototype, "src");
  const currentSrc = findGetter(mediaPrototype, "currentSrc");
  const paused = findGetter(mediaPrototype, "paused");
  const ended = findGetter(mediaPrototype, "ended");
  const readyState = findGetter(mediaPrototype, "readyState");
  const currentTime = findGetter(mediaPrototype, "currentTime");
  const srcObject = findOptionalGetter(mediaPrototype, "srcObject");
  const duration = findGetter(mediaPrototype, "duration");
  const muted = findGetter(mediaPrototype, "muted");
  const volume = findGetter(mediaPrototype, "volume");
  const loop = findGetter(mediaPrototype, "loop");
  const mediaError = findOptionalGetter(mediaPrototype, "error");
  // Оригинальный геттер `MediaError.prototype.code`: страница не подменит ответ «какая ошибка».
  const mediaErrorCode = findOptionalGetter(win.MediaError?.prototype, "code");
  // Нет в небезопасном контексте (http): там ключей быть не может.
  const mediaKeys = findOptionalGetter(mediaPrototype, "mediaKeys");
  const visibility = captureVisibilityReaders(win);
  const children = findOptionalGetter(win.Element.prototype, "children");
  const localName = findOptionalGetter(win.Element.prototype, "localName");
  const getAttribute = findMethod(win.Element.prototype, "getAttribute");
  const sourceSrc = findOptionalGetter(win.HTMLSourceElement?.prototype, "src");
  const isMediaStream = hasPrototypeOf(win.MediaStream?.prototype);
  const isBlob = hasPrototypeOf(win.Blob?.prototype);
  const documentObject = win.document;
  // Происхождение документа за его жизнь не меняется; снимаем здесь, до скриптов страницы.
  const capturedOrigin = win.location?.origin;
  const documentOrigin = typeof capturedOrigin === "string" && capturedOrigin !== OPAQUE_ORIGIN ? capturedOrigin : null;

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
    readyStateOf: (element) => Number(readyState(element)),
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
    durationSecondsOf: (element) => {
      const seconds = duration(element);
      return typeof seconds === "number" ? seconds : Number.NaN;
    },
    isSilent: (element) => muted(element) === true || volume(element) === 0,
    isLooping: (element) => loop(element) === true,
    videoSizeOf: (element) => {
      const width = videoWidth(element);
      const height = videoHeight(element);
      return { width: typeof width === "number" ? width : 0, height: typeof height === "number" ? height : 0 };
    },
    hasPlaybackFailureError: (element) => {
      if (mediaError === undefined) return false;
      const error = mediaError(element);
      if (error == null) return false;
      const code = mediaErrorCode !== undefined ? mediaErrorCode(error) : (error as { readonly code?: unknown }).code;
      return code === MEDIA_ERR_NETWORK || code === MEDIA_ERR_DECODE;
    },
    hasMediaKeys: (element) => mediaKeys !== undefined && mediaKeys(element) != null,
    documentHostName: () => {
      const host = win.location?.hostname;
      return typeof host === "string" ? host : "";
    },
    documentOrigin: () => documentOrigin,
    referrerPolicyOf: (element) => {
      const policy = getAttribute(element, "referrerpolicy");
      return typeof policy === "string" ? policy : null;
    },
    metaReferrerContents: () => {
      const contents: string[] = [];
      for (const meta of toElementArray(documentQuery(documentObject, META_SELECTOR))) {
        const name = getAttribute(meta, "name");
        const content = getAttribute(meta, "content");
        if (typeof name === "string" && name.toLowerCase() === REFERRER_META_NAME && typeof content === "string") contents.push(content);
      }
      return contents;
    },
    createVisibilityProbe: () => visibility.createProbe(),
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
