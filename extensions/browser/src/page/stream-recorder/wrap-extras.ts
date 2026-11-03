// Обёртки записи, которых нет в общем `prototype-wrap.ts`: сеттер с СЫРЫМ значением страницы и
// метод, чей сбой тоже нужен записи. Правила те же: оригинал первым, исключение оригинала уходит
// странице как было, наблюдатель никогда не бросает в страницу.

import { observeSafely, type OddityReporter } from "../prototype-wrap.ts";

/**
 * Обернуть сеттер, передав наблюдателю то, что написала страница (а не нормализованное браузером
 * значение). Нужен элементу: `currentTime = 12.5` для записи — именно 12.5.
 */
export function wrapSetterRaw(owner: object, name: string, reportOddity: OddityReporter, onSet: (target: unknown, written: unknown) => void): boolean {
  const descriptor = Object.getOwnPropertyDescriptor(owner, name);
  const originalSet = descriptor?.set;
  if (descriptor === undefined || originalSet === undefined) return false;
  const holder = {
    set [name](value: unknown) {
      Reflect.apply(originalSet, this, [value]);
      observeSafely(reportOddity, name, () => onSet(this, value));
    },
  };
  const wrappedSet = Object.getOwnPropertyDescriptor(holder, name)?.set;
  if (wrappedSet === undefined) return false;
  Object.defineProperty(owner, name, { ...descriptor, set: wrappedSet });
  return true;
}

/** Обработчики вызова метода: успех (результат оригинала) и сбой (исключение оригинала). */
export interface CallOutcomeObserver {
  onReturned(target: unknown, args: readonly unknown[], result: unknown): void;
  onThrown(target: unknown, args: readonly unknown[], error: unknown): void;
}

/** Обернуть метод так, чтобы наблюдатель узнал и об успехе, и о сбое; сбой уходит странице как был. */
export function wrapMethodWithOutcome(owner: object, name: string, reportOddity: OddityReporter, observer: CallOutcomeObserver): boolean {
  const descriptor = Object.getOwnPropertyDescriptor(owner, name);
  const original: unknown = descriptor?.value;
  if (descriptor === undefined || typeof original !== "function") return false;
  const wrapper = {
    [name](this: unknown, ...args: unknown[]): unknown {
      let result: unknown;
      try {
        result = Reflect.apply(original, this, args);
      } catch (error) {
        observeSafely(reportOddity, name, () => observer.onThrown(this, args, error));
        throw error;
      }
      observeSafely(reportOddity, name, () => observer.onReturned(this, args, result));
      return result;
    },
  }[name];
  if (wrapper === undefined) return false;
  Object.defineProperty(wrapper, "length", { value: (original as { readonly length: number }).length });
  Object.defineProperty(owner, name, { ...descriptor, value: wrapper });
  return true;
}
