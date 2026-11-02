// Общие правила обёрток над методами и сеттерами страницы (MSE, `URL.createObjectURL`,
// `attachShadow`).
//
//  - сначала оригинал (через `Reflect.apply`), наблюдатель узнаёт только об УСПЕШНОМ вызове;
//  - исключение оригинала уходит странице ровно как было — наблюдатель не зовётся;
//  - наблюдатель никогда не бросает в страницу: сбой ловится, о нём — одна отладочная запись;
//  - «оригинал» — то, что лежит на объекте в момент установки (document_start). Если до нас его
//    подменил блокировщик, мы оборачиваем его обёртку; если подменит после нас — его обёртка
//    позовёт нашу. В обоих порядках вызов страницы проходит через обоих;
//  - обёртка похожа на оригинал: то же имя и `length`, не конструктор, те же флаги свойства.

/** Куда сообщить о сбое нашего наблюдения (не страницы). */
export type OddityReporter = (what: string) => void;

/** Что делает наблюдатель после успешного вызова: `this`, аргументы и результат. */
export type AfterCall = (target: unknown, args: readonly unknown[], result: unknown) => void;

/** Выполнить наблюдение так, чтобы оно никогда не бросило в страницу. */
export function observeSafely(reportOddity: OddityReporter, what: string, observe: () => void): void {
  try {
    observe();
  } catch {
    try {
      reportOddity(`observation failed: ${what}`);
    } catch {
      // Даже отладочная запись не должна ронять страницу.
    }
  }
}

/** Обернуть метод объекта (прототипа или конструктора). `false` — метода нет, ничего не тронуто. */
export function wrapMethod(owner: object, name: string, reportOddity: OddityReporter, afterCall: AfterCall): boolean {
  const descriptor = Object.getOwnPropertyDescriptor(owner, name);
  const original: unknown = descriptor?.value;
  if (descriptor === undefined || typeof original !== "function") return false;
  // Метод из литерала объекта: его имя — `name`, как у оригинала, и он не конструктор.
  const wrapper = {
    [name](this: unknown, ...args: unknown[]): unknown {
      const result: unknown = Reflect.apply(original, this, args);
      observeSafely(reportOddity, name, () => afterCall(this, args, result));
      return result;
    },
  }[name];
  if (wrapper === undefined) return false;
  Object.defineProperty(wrapper, "length", { value: (original as { readonly length: number }).length });
  Object.defineProperty(owner, name, { ...descriptor, value: wrapper });
  return true;
}

/**
 * Обернуть сеттер свойства-аксессора. Наблюдатель получает значение, прочитанное геттером ПОСЛЕ
 * записи: браузер сам нормализует число (так мы видим ровно то, что и страница).
 */
export function wrapSetter(
  owner: object,
  name: string,
  reportOddity: OddityReporter,
  onChanged: (target: unknown, normalizedValue: unknown) => void,
): boolean {
  const descriptor = Object.getOwnPropertyDescriptor(owner, name);
  const originalSet = descriptor?.set;
  const originalGet = descriptor?.get;
  if (descriptor === undefined || originalSet === undefined || originalGet === undefined) return false;
  // Сеттер из литерала объекта: его имя «set <name>», как у оригинала.
  const holder = {
    set [name](value: unknown) {
      Reflect.apply(originalSet, this, [value]);
      observeSafely(reportOddity, name, () => onChanged(this, Reflect.apply(originalGet, this, [])));
    },
  };
  const wrappedSet = Object.getOwnPropertyDescriptor(holder, name)?.set;
  if (wrappedSet === undefined) return false;
  Object.defineProperty(owner, name, { ...descriptor, set: wrappedSet });
  return true;
}

/** Объект ли это (цель вызова или результат), а не примитив. */
export function isObject(candidate: unknown): candidate is object {
  return (typeof candidate === "object" || typeof candidate === "function") && candidate !== null;
}
