// Контракт руки-функции на стороне MAIN-мира (сессия 14b, решение 282): белый список действий адаптера.
//
// Действие — маленькая функция, которая находит функцию плеера сайта в окне страницы и зовёт её. Изолированный
// мир называет только имя действия из этого списка: «вызови что угодно» попросить нельзя
// (`shared/adapter-hand-contract.ts`). Результат действия MAIN-мир копирует в `HandValue` и отдаёт обратно.

/** Ответ действия «у сайта нет такой функции»: страница не та или плеер ещё не построен. */
export const PAGE_FUNCTION_MISSING: unique symbol = Symbol("page function missing");

/**
 * Одно действие руки. `page` — окно страницы (MAIN-мир). Возвращает результат функции сайта (может быть
 * обещанием) или `PAGE_FUNCTION_MISSING`. Может бросить: это ловит исполнитель (`src/page/adapter-hand.ts`).
 */
export type PlayerHandAction = (page: object, argument: string | null) => unknown;

/** Белый список действий одного адаптера: имя действия → действие. */
export type PlayerHandActions = Readonly<Record<string, PlayerHandAction>>;

/**
 * Найти функцию объекта страницы и вызвать её с её же `this`. Нет объекта или функции — `PAGE_FUNCTION_MISSING`.
 * Читается каждый раз заново: страница вправе пересоздать свой плеер.
 */
export function callPageFunction(page: object, objectName: string, functionName: string, ...functionArguments: readonly unknown[]): unknown {
  const target: unknown = Reflect.get(page, objectName);
  if (typeof target !== "object" || target === null) return PAGE_FUNCTION_MISSING;
  const method: unknown = Reflect.get(target, functionName);
  if (typeof method !== "function") return PAGE_FUNCTION_MISSING;
  return Reflect.apply(method, target, functionArguments);
}
