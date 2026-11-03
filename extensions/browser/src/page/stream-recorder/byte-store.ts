// Склад байтов записи: копии всех отданных `appendBuffer` кусков подряд, с потолком.
//
// Копия берётся в момент записи (страница может переиспользовать свой буфер). Дошли до потолка —
// склад выставляет `truncated` и больше байтов не принимает: хвост не записывается, а события без
// байтов продолжают писаться (решает `StreamRecorder`).

/** Результат попытки положить кусок. */
export type StoreResult =
  /** Кусок лежит на складе с этого смещения. */
  | { readonly kind: "stored"; readonly offset: number }
  /** Потолок исчерпан: кусок не записан. */
  | { readonly kind: "dropped" };

export class ByteStore {
  readonly #ceilingBytes: number;
  #chunks: Uint8Array[] = [];
  #sizeBytes = 0;
  #truncated = false;

  constructor(ceilingBytes: number) {
    this.#ceilingBytes = ceilingBytes;
  }

  /** Положить копию куска. Склад, однажды упёршийся в потолок, больше ничего не принимает. */
  append(bytes: Uint8Array): StoreResult {
    if (this.#truncated || this.#sizeBytes + bytes.length > this.#ceilingBytes) {
      this.#truncated = true;
      return { kind: "dropped" };
    }
    const offset = this.#sizeBytes;
    this.#chunks.push(bytes.slice());
    this.#sizeBytes += bytes.length;
    return { kind: "stored", offset };
  }

  get sizeBytes(): number {
    return this.#sizeBytes;
  }

  get truncated(): boolean {
    return this.#truncated;
  }

  get chunks(): readonly Uint8Array[] {
    return this.#chunks;
  }

  /** Отдать накопленное и начать заново (воркер сбрасывает склад после каждой отправки). */
  drain(): { readonly chunks: Uint8Array[]; readonly sizeBytes: number } {
    const drained = { chunks: this.#chunks, sizeBytes: this.#sizeBytes };
    this.#chunks = [];
    this.#sizeBytes = 0;
    return drained;
  }
}
