// Байты, которые страница скормила `appendBuffer`, — без копирования (решение 176).
//
// `appendBuffer` принимает `ArrayBuffer` или любой вид на него (`Uint8Array`, `DataView`, …).
// Нам нужен единый вид `Uint8Array` ровно на тот диапазон, который увидел браузер:
//  - `ArrayBuffer` → вид на весь буфер;
//  - вид → вид с тем же смещением и длиной (а не весь `buffer` целиком!).
// Ничего не копируется: вид ссылается на память страницы.
//
// Внутренние поля (`byteOffset`, `byteLength`, `buffer`) читаются геттерами, снятыми с прототипов
// на document_start: страница позже может подменить свойства своего объекта или прототипа, а
// браузер внутри `appendBuffer` читает именно внутренние слоты. Геттер чужого «бренда» бросает
// исключение — это и есть проверка типа без `instanceof` (работает и между фреймами).

/** Конструкторы и геттеры двоичных типов, снятые на document_start. */
export interface BinaryIntrinsics {
  readonly Uint8Array: Uint8ArrayConstructor;
  readonly arrayBufferByteLength: (target: unknown) => number;
  readonly typedArrayBuffer: (target: unknown) => ArrayBufferLike;
  readonly typedArrayByteOffset: (target: unknown) => number;
  readonly typedArrayByteLength: (target: unknown) => number;
  readonly dataViewBuffer: (target: unknown) => ArrayBufferLike;
  readonly dataViewByteOffset: (target: unknown) => number;
  readonly dataViewByteLength: (target: unknown) => number;
  readonly typedArraySlice: (target: Uint8Array) => Uint8Array;
}

/** Окно с нужными для байтов конструкторами (в тестах — объект с глобалами Node). */
export interface BinaryWindow {
  readonly Uint8Array: Uint8ArrayConstructor;
  readonly ArrayBuffer: ArrayBufferConstructor;
  readonly DataView: DataViewConstructor;
}

/** Геттер свойства прототипа как обычная функция (вызов через Reflect.apply). */
function captureGetter<T>(prototype: object, name: string): (target: unknown) => T {
  const descriptor = Object.getOwnPropertyDescriptor(prototype, name);
  const getter = descriptor?.get;
  if (getter === undefined) {
    // Такого в браузерах нет; если всё же случилось — любое чтение считается «не тот тип».
    return () => {
      throw new TypeError(`no getter ${name}`);
    };
  }
  return (target) => Reflect.apply(getter, target, []) as T;
}

/** Снять двоичные геттеры и конструкторы окна. Зовётся один раз на document_start. */
export function captureBinaryIntrinsics(win: BinaryWindow): BinaryIntrinsics {
  // %TypedArray%.prototype — общий прототип всех типизированных массивов.
  const typedArrayPrototype = Object.getPrototypeOf(win.Uint8Array.prototype) as object;
  const slice = Object.getOwnPropertyDescriptor(typedArrayPrototype, "slice")?.value as (this: Uint8Array) => Uint8Array;
  return {
    Uint8Array: win.Uint8Array,
    arrayBufferByteLength: captureGetter<number>(win.ArrayBuffer.prototype, "byteLength"),
    typedArrayBuffer: captureGetter<ArrayBufferLike>(typedArrayPrototype, "buffer"),
    typedArrayByteOffset: captureGetter<number>(typedArrayPrototype, "byteOffset"),
    typedArrayByteLength: captureGetter<number>(typedArrayPrototype, "byteLength"),
    dataViewBuffer: captureGetter<ArrayBufferLike>(win.DataView.prototype, "buffer"),
    dataViewByteOffset: captureGetter<number>(win.DataView.prototype, "byteOffset"),
    dataViewByteLength: captureGetter<number>(win.DataView.prototype, "byteLength"),
    typedArraySlice: (target) => Reflect.apply(slice, target, []) as Uint8Array,
  };
}

/** Пробует прочитать значение; чужой «бренд» (геттер бросил) → `undefined`. */
function tryRead<T>(read: () => T): T | undefined {
  try {
    return read();
  } catch {
    return undefined;
  }
}

/**
 * Вид `Uint8Array` на байты аргумента `appendBuffer` БЕЗ копирования.
 * `null` — аргумент не `ArrayBuffer` и не вид на него (браузер такой вызов отверг бы сам).
 */
export function viewOfAppendSource(intrinsics: BinaryIntrinsics, source: unknown): Uint8Array | null {
  if (typeof source !== "object" || source === null) return null;
  const wholeBufferLength = tryRead(() => intrinsics.arrayBufferByteLength(source));
  if (wholeBufferLength !== undefined) {
    return new intrinsics.Uint8Array(source as ArrayBuffer, 0, wholeBufferLength);
  }
  const typedBuffer = tryRead(() => intrinsics.typedArrayBuffer(source));
  if (typedBuffer !== undefined) {
    return new intrinsics.Uint8Array(typedBuffer, intrinsics.typedArrayByteOffset(source), intrinsics.typedArrayByteLength(source));
  }
  const dataViewBuffer = tryRead(() => intrinsics.dataViewBuffer(source));
  if (dataViewBuffer !== undefined) {
    return new intrinsics.Uint8Array(dataViewBuffer, intrinsics.dataViewByteOffset(source), intrinsics.dataViewByteLength(source));
  }
  return null;
}

/**
 * Копия вида в собственный плотный буфер — ровно его байты.
 *
 * Зачем своя копия, хотя порт и так клонирует сообщение: структурное клонирование вида копирует
 * ВЕСЬ его `buffer`, а не только диапазон (кусок посреди 10-МиБ загрузки ушёл бы целиком). Плотную
 * копию мы отдаём порту в списке transfer — значит, копия ровно одна, а память страницы не трогаем.
 */
export function copyForTransfer(intrinsics: BinaryIntrinsics, view: Uint8Array): Uint8Array {
  return intrinsics.typedArraySlice(view);
}
