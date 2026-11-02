// Метка документа (`DocumentTag`, договор): случайная строка, которую MAIN-мир выбирает один раз
// на старте документа и кладёт в каждого кандидата `describe_candidates`.
//
// Зачем: между опросом кандидатов и `begin_handoff` фон ждёт плеер (секунды). За это время фрейм
// может перейти на другой документ, где ключи элементов снова начинаются с 1, — и команда попала
// бы в чужой `<video>`. Команда несёт метку документа, из которого выбран элемент; чужая метка →
// `element_not_found`.

import type { DocumentTag } from "../shared/handoff-contract.ts";

/**
 * Случайных байтов в метке: 16 (128 бит). Договор требует ≥ 64 бит; 128 — с запасом, чтобы
 * совпадение меток двух документов одной вкладки было невозможно на практике.
 */
export const DOCUMENT_TAG_RANDOM_BYTES = 16;

/** Окно с криптографическим генератором (в тестах — `globalThis` Node). */
export interface RandomWindow {
  readonly crypto: { getRandomValues(array: Uint8Array<ArrayBuffer>): unknown };
}

/** Снятый на document_start `crypto.getRandomValues` (страница позже может его подменить). */
export type RandomFill = (bytes: Uint8Array) => void;

export function captureRandomFill(win: RandomWindow): RandomFill {
  const cryptoObject = win.crypto;
  const getRandomValues = cryptoObject.getRandomValues;
  return (bytes) => {
    Reflect.apply(getRandomValues, cryptoObject, [bytes]);
  };
}

/** Новая метка документа: 32 шестнадцатеричные цифры (hex входит в алфавит base64url договора). */
export function createDocumentTag(fill: RandomFill): DocumentTag {
  const bytes = new Uint8Array(DOCUMENT_TAG_RANDOM_BYTES);
  fill(bytes);
  let tag = "";
  for (const byte of bytes) tag += byte.toString(16).padStart(2, "0");
  return tag;
}
