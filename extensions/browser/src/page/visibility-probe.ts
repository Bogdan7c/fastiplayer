// Настоящая видимость элемента для выбора видео (решение 68): скрытый плеер не должен побеждать
// видимый. Геометрии (`getBoundingClientRect`) для этого мало: `visibility:hidden` и `opacity:0`
// оставляют элементу полный прямоугольник, а страницы Ashdi именно так прячут играющий плеер.
//
// «Скрыт», если у самого элемента или любого предка:
//  - `display:none` (нет прямоугольника);
//  - `opacity:0` (не наследуется, поэтому смотрим каждого предка);
//  - `visibility:hidden` / `collapse` (наследуется: достаточно значения самого элемента, потому что
//    потомок с `visibility:visible` внутри скрытого предка виден — это правило CSS).
//
// Стоимость опроса (ТРЕБОВАНИЕ «не квадратично»): страница с сотнями `<video>` в одном дереве не
// должна заставлять нас перечитывать стили общих предков для каждого видео.
//  1. Основной путь — `Element.checkVisibility(...)`: один встроенный вызов на элемент, браузер сам
//     обходит предков по своим готовым данным о раскладке. O(число видео).
//  2. Запасной путь (старый движок): обход вверх через `getComputedStyle`, но с памятью ответов
//     «предок скрыт?» на время одного опроса (`createProbe` даёт новый кэш). Каждый узел дерева
//     читается не больше одного раза за опрос: O(число различных предков), а не O(видео × глубина).
// Все чтения — оригиналы, снятые на document_start: страница не может подменить ни метод, ни стиль.
// Любой сбой чтения (чужой «бренд», бросающий геттер) трактуется как «не скрыт»: хуже пропустить
// скрытый плеер, чем «потерять» видимое видео из-за мусора в чтении.

/** Окно с тем, что нужно пробе (в тестах — фейк). Всё необязательное: нет — путь не работает. */
export interface VisibilityWindow {
  readonly Element: { readonly prototype: object };
  readonly Node: { readonly prototype: object };
  readonly getComputedStyle?: (element: never) => unknown;
  readonly CSSStyleDeclaration?: { readonly prototype: object };
  readonly ShadowRoot?: { readonly prototype: object };
}

/** Проба одного опроса: `true` — элемент скрыт. Держит кэш только на свою жизнь. */
export type VisibilityProbe = (element: object) => boolean;

/** Читатели видимости, снятые один раз на document_start. */
export interface VisibilityReaders {
  /** Новая проба с чистым кэшем — на один опрос документа. */
  createProbe(): VisibilityProbe;
}

/** Значения `visibility`, при которых элемент не рисуется. */
const HIDDEN_VISIBILITY_VALUES: readonly string[] = ["hidden", "collapse"];

/** Параметры `checkVisibility`: оба имени (современное и историческое) — для старых движков. */
const CHECK_VISIBILITY_OPTIONS = {
  opacityProperty: true,
  visibilityProperty: true,
  checkOpacity: true,
  checkVisibilityCSS: true,
} as const;

/** Метод по цепочке прототипов (`undefined`, если у движка его нет). */
function findOptionalMethod(prototype: object | undefined, name: string): ((target: unknown, ...args: unknown[]) => unknown) | undefined {
  let current: object | null = prototype ?? null;
  while (current !== null) {
    const method: unknown = Object.getOwnPropertyDescriptor(current, name)?.value;
    if (typeof method === "function") return (target, ...args) => Reflect.apply(method, target, args);
    current = Object.getPrototypeOf(current) as object | null;
  }
  return undefined;
}

/** Геттер по цепочке прототипов (`undefined`, если у движка его нет). */
function findOptionalGetter(prototype: object | undefined, name: string): ((target: unknown) => unknown) | undefined {
  let current: object | null = prototype ?? null;
  while (current !== null) {
    const getter = Object.getOwnPropertyDescriptor(current, name)?.get;
    if (getter !== undefined) return (target) => Reflect.apply(getter, target, []);
    current = Object.getPrototypeOf(current) as object | null;
  }
  return undefined;
}

/** Снять оригиналы и собрать читателей. Зовётся один раз на document_start. */
export function captureVisibilityReaders(win: VisibilityWindow): VisibilityReaders {
  const checkVisibility = findOptionalMethod(win.Element.prototype, "checkVisibility");
  const computedStyleOf = win.getComputedStyle?.bind(win);
  const getPropertyValue = findOptionalMethod(win.CSSStyleDeclaration?.prototype, "getPropertyValue");
  const parentNode = findOptionalGetter(win.Node.prototype, "parentNode");
  const shadowHost = findOptionalGetter(win.ShadowRoot?.prototype, "host");

  /** Одно значение вычисленного стиля узла; `""` — прочитать не вышло. */
  const styleValue = (node: object, property: string): string => {
    if (computedStyleOf === undefined || getPropertyValue === undefined) return "";
    const value = getPropertyValue(computedStyleOf(node as never), property);
    return typeof value === "string" ? value.trim().toLowerCase() : "";
  };

  /** Родитель в «плоском» смысле: хозяин shadow root для корня shadow, иначе `parentNode`. */
  const parentOf = (node: object): object | null => {
    const parent = parentNode?.(node);
    if (typeof parent !== "object" || parent === null) return null;
    const host = shadowHost?.(parent);
    return typeof host === "object" && host !== null ? host : parent;
  };

  /** Сам узел скрывает себя и потомков: `display:none` или нулевая непрозрачность. */
  const hidesItself = (node: object): boolean => {
    // Не-элемент в цепочке (документ, фрагмент) стиля не имеет: `getComputedStyle` на нём бросает.
    // Это «не скрывает», а не «опрос сломан»: идём к следующему предку.
    try {
      if (styleValue(node, "display") === "none") return true;
      const opacity = styleValue(node, "opacity");
      return opacity !== "" && Number.parseFloat(opacity) === 0;
    } catch {
      return false;
    }
  };

  return {
    createProbe: () => {
      // Кэш запасного пути: «этот узел или кто-то из его предков скрывает поддерево».
      const hiddenByChain = new WeakMap<object, boolean>();
      /** Итеративно (без рекурсии: глубина DOM у страницы-врага не должна ронять стек). */
      const chainHides = (start: object): boolean => {
        const visited: object[] = [];
        let verdict = false;
        let node: object | null = start;
        while (node !== null) {
          const known = hiddenByChain.get(node);
          if (known !== undefined) {
            verdict = known;
            break;
          }
          if (hidesItself(node)) {
            verdict = true;
            hiddenByChain.set(node, true);
            break;
          }
          visited.push(node);
          node = parentOf(node);
        }
        // Все пройденные узлы получают тот же итог: до них дошла (или не дошла) скрывающая цепочка.
        for (const seen of visited) hiddenByChain.set(seen, verdict);
        return verdict;
      };

      return (element) => {
        try {
          if (checkVisibility !== undefined) return checkVisibility(element, CHECK_VISIBILITY_OPTIONS) === false;
          if (HIDDEN_VISIBILITY_VALUES.includes(styleValue(element, "visibility"))) return true;
          return chainHides(element);
        } catch {
          return false;
        }
      };
    },
  };
}
