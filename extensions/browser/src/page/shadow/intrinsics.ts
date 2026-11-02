// Встроенные функции языка, снятые при загрузке скрипта MAIN-мира (document_start).
//
// Зачем: скрипт страницы работает в том же глобале и может позже подменить `Reflect.apply`
// или методы `WeakMap.prototype`. Тень зовёт оригиналы и хранит свои записи через эти копии,
// поэтому подмена страницы не видит наших вызовов и не ломает тень.

/** `Reflect.apply` на момент загрузки скрипта. */
export const applyOriginal: typeof Reflect.apply = Reflect.apply;

const isPrototypeOf = Object.prototype.isPrototypeOf;

/** `prototype` лежит в цепочке прототипов `candidate` (как `instanceof`, но без подменяемых глобалов). */
export function hasPrototype(candidate: unknown, prototype: object): boolean {
  return applyOriginal(isPrototypeOf, prototype, [candidate]) === true;
}

const weakMapGet = WeakMap.prototype.get;
const weakMapSet = WeakMap.prototype.set;
const weakMapDelete = WeakMap.prototype.delete;

/**
 * Слабая таблица «объект → значение», которую нельзя подсмотреть или испортить подменой
 * `WeakMap.prototype`. Ключ — любой объект; для не-объекта `get` честно отвечает `undefined`.
 */
export class SealedWeakMap<Key extends object, Value> {
  readonly #table = new WeakMap<Key, Value>();

  get(key: unknown): Value | undefined {
    // WeakMap.get с примитивом возвращает undefined и не бросает — этим пользуются
    // переопределения свойств, которые страница может позвать на чём угодно.
    return applyOriginal(weakMapGet, this.#table, [key]) as Value | undefined;
  }

  set(key: Key, value: Value): void {
    applyOriginal(weakMapSet, this.#table, [key, value]);
  }

  delete(key: Key): void {
    applyOriginal(weakMapDelete, this.#table, [key]);
  }
}
