// Байтовые операции наблюдателя субтитров на оригиналах, снятых на document_start.
//
// `TypedArray.prototype.set`, `TextEncoder`, `JSON.stringify` страница может подменить — в том числе
// так, чтобы подсмотреть или исказить документ. Поэтому всё нужное снимается заранее и зовётся
// через `Reflect.apply`.

import { copyForTransfer, viewOfAppendSource, type BinaryIntrinsics } from "../binary-view.ts";

/** Окно с нужными конструкторами; кого нет — соответствующая операция недоступна (`null`). */
export interface ByteToolsWindow {
  readonly Uint8Array: Uint8ArrayConstructor;
  readonly TextEncoder?: { new (): object; readonly prototype: object };
  readonly JSON?: { readonly stringify: (value: unknown) => string };
}

export interface ByteTools {
  /** Склеить куски в один буфер ровно своего размера. */
  join(chunks: readonly Uint8Array[], totalBytes: number): Uint8Array;
  /** UTF-8 байты строки; `null` — кодировщика в окне нет. */
  encodeUtf8: ((text: string) => Uint8Array) | null;
  /** `JSON.stringify` значения; `null` — нет в окне или результата нет (`undefined`, цикл). */
  stringifyJson: ((value: unknown) => string | null) | null;
  /** Собственная плотная копия байт `ArrayBuffer`/вида страницы; `null` — это не байты. */
  copyOf(source: unknown): Uint8Array | null;
  /** Длина вида в байтах, снятым геттером. */
  lengthOf(view: Uint8Array): number;
}

function captureTypedArraySet(win: ByteToolsWindow): (target: Uint8Array, source: Uint8Array, offset: number) => void {
  const typedArrayPrototype = Object.getPrototypeOf(win.Uint8Array.prototype) as object;
  const set = Object.getOwnPropertyDescriptor(typedArrayPrototype, "set")?.value as Function | undefined;
  if (typeof set !== "function") throw new TypeError("TypedArray.prototype.set is missing");
  return (target, source, offset) => void Reflect.apply(set, target, [source, offset]);
}

export function captureByteTools(win: ByteToolsWindow, binary: BinaryIntrinsics): ByteTools {
  const set = captureTypedArraySet(win);
  let encodeUtf8: ByteTools["encodeUtf8"] = null;
  if (win.TextEncoder !== undefined) {
    const encode = Object.getOwnPropertyDescriptor(win.TextEncoder.prototype, "encode")?.value as Function | undefined;
    const encoder = new win.TextEncoder();
    if (typeof encode === "function") encodeUtf8 = (text) => Reflect.apply(encode, encoder, [text]) as Uint8Array;
  }
  let stringifyJson: ByteTools["stringifyJson"] = null;
  const stringify = win.JSON?.stringify;
  if (typeof stringify === "function") {
    const original = stringify as Function;
    const json = win.JSON;
    stringifyJson = (value) => {
      try {
        const text: unknown = Reflect.apply(original, json, [value]);
        return typeof text === "string" ? text : null;
      } catch {
        return null;
      }
    };
  }
  return {
    join(chunks, totalBytes) {
      const joined = new binary.Uint8Array(totalBytes);
      let offset = 0;
      for (const chunk of chunks) {
        set(joined, chunk, offset);
        offset += binary.typedArrayByteLength(chunk);
      }
      return joined;
    },
    encodeUtf8,
    stringifyJson,
    copyOf(source) {
      const view = viewOfAppendSource(binary, source);
      return view === null ? null : copyForTransfer(binary, view);
    },
    lengthOf: (view) => binary.typedArrayByteLength(view),
  };
}
