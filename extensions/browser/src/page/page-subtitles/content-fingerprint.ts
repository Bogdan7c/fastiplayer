// Отпечаток содержимого документа: «тот же документ или другой» без хранения второй копии.
//
// Два независимых 32-битных хеша (FNV-1a и djb2) плюс длина: случайное совпадение всех трёх у разных
// документов практически невозможно. Это не защита от злого умысла: страница, подобравшая коллизию,
// добьётся лишь того, что два её же документа сольются в одну дорожку — вреда сеансу нет.
// Хеши считаются целочисленно (`Math.imul`), без криптографии страницы (`crypto.subtle` подменяем).

/** Отпечаток: длина и два хеша. */
export interface ContentFingerprint {
  readonly length: number;
  readonly fnv1a: number;
  readonly djb2: number;
}

/** Начальное значение и множитель FNV-1a, 32 бита (Fowler–Noll–Vo). */
const FNV_OFFSET_BASIS = 0x811c9dc5;
const FNV_PRIME = 0x01000193;

/** Начальное значение и множитель djb2 (Бернштейн). */
const DJB2_START = 5381;
const DJB2_MULTIPLIER = 33;

export function fingerprintOf(bytes: Uint8Array): ContentFingerprint {
  let fnv = FNV_OFFSET_BASIS;
  let djb = DJB2_START;
  for (let index = 0; index < bytes.length; index += 1) {
    const byte = bytes[index] ?? 0;
    fnv = Math.imul(fnv ^ byte, FNV_PRIME) >>> 0;
    djb = (Math.imul(djb, DJB2_MULTIPLIER) + byte) >>> 0;
  }
  return { length: bytes.length, fnv1a: fnv, djb2: djb };
}

export function isSameContent(left: ContentFingerprint, right: ContentFingerprint): boolean {
  return left.length === right.length && left.fnv1a === right.fnv1a && left.djb2 === right.djb2;
}
