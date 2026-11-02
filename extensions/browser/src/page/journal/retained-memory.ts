// Честный счёт памяти, которую журнал держит ссылками (решение 176).
//
// Журнал хранит не байты, а виды на память страницы. Пока жив хоть один наш вид, браузер не может
// освободить ВЕСЬ буфер под ним — поэтому считаем не длину видов, а размер буферов, на которые они
// ссылаются, и каждый буфер — один раз (страница часто кладёт подряд куски одной большой загрузки:
// десять видов по 1 МиБ на один буфер 10 МиБ держат 10 МиБ, а не 1 и не 100).

export class RetainedMemory {
  /** Буфер → сколько наших видов на него ссылается и сколько он весил, когда мы его взяли. */
  readonly #buffers = new Map<ArrayBufferLike, { references: number; readonly bytes: number }>();
  #totalBytes = 0;

  /** Сколько байтов буферов страницы сейчас держат наши виды. */
  get totalBytes(): number {
    return this.#totalBytes;
  }

  /** Вид начал ссылаться на свой буфер. */
  retain(view: Uint8Array): void {
    const buffer = view.buffer;
    const known = this.#buffers.get(buffer);
    if (known !== undefined) {
      known.references += 1;
      return;
    }
    // Размер запоминаем сейчас: если страница потом «отсоединит» буфер (transfer в воркер), его
    // byteLength станет 0, а вычесть надо ровно то, что прибавили.
    const bytes = buffer.byteLength;
    this.#buffers.set(buffer, { references: 1, bytes });
    this.#totalBytes += bytes;
  }

  /** Вид больше не нужен журналу. */
  release(view: Uint8Array): void {
    const buffer = view.buffer;
    const known = this.#buffers.get(buffer);
    if (known === undefined) return;
    known.references -= 1;
    if (known.references > 0) return;
    this.#buffers.delete(buffer);
    this.#totalBytes -= known.bytes;
  }
}
