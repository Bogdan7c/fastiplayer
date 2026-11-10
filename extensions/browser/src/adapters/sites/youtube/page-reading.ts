// Мелкие помощники чтения чужого плеера YouTube в MAIN-мире: общие для `player-hand.ts` и `captions-hand.ts`.
// Страница — враг (инвариант 1): всё, что читаем, проверяем по форме и не доверяем.

import { callPageFunction } from "../../capabilities/player-hand/contract.ts";

/** Читает `объект[имя]` как есть; не объект — `undefined`. */
export function read(target: unknown, name: string): unknown {
  return typeof target === "object" && target !== null ? Reflect.get(target, name) : undefined;
}

/** Зовёт функцию объекта-плеера, если она есть; иначе `PAGE_FUNCTION_MISSING`. */
export function call(player: object, name: string, ...args: readonly unknown[]): unknown {
  return callPageFunction({ player }, "player", name, ...args);
}

/** Строка непустая? */
export function nonEmptyString(candidate: unknown): candidate is string {
  return typeof candidate === "string" && candidate !== "";
}

/** Список из ответа функции плеера; не список (или функции нет) — пустой список. */
export function listFrom(value: unknown): readonly unknown[] {
  return Array.isArray(value) ? value : [];
}
