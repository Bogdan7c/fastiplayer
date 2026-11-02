// Целость вида на память страницы (решение 176, «память без копий»).
//
// Журнал держит ВИДЫ на буферы страницы, а не копии. Страница вправе потом распорядиться своим
// буфером: передать его в воркер (`postMessage(..., [buffer])`) или уменьшить resizable-буфер.
// Тогда вид «пустеет» (byteLength 0) — и повтор молча потерял бы кусок, а счёт смещений журнала
// разъехался бы. Поэтому при записи запоминаем длины, а перед повтором сверяем их.
//
// Чего так поймать нельзя: страница переписала байты в том же буфере (переиспользует буфер под
// следующую загрузку), не отсоединяя его. Это цена отказа от копий до передачи — решение 176
// владельца; повтор тогда отдаст новые байты, плеер их отвергнет как неразборчивые или покажет
// сбой поколения, но страница при этом не страдает.

/** Вид и его длины в момент записи. */
export interface RecordedView {
  readonly view: Uint8Array;
  /** Длина вида при записи. */
  readonly length: number;
  /** Длина всего буфера при записи. */
  readonly bufferLength: number;
}

export function recordView(view: Uint8Array): RecordedView {
  return { view, length: view.byteLength, bufferLength: view.buffer.byteLength };
}

/** Вид всё ещё смотрит на те же байты той же длины (буфер не отсоединён и не урезан). */
export function isViewIntact(recorded: RecordedView): boolean {
  return recorded.view.byteLength === recorded.length && recorded.view.buffer.byteLength === recorded.bufferLength;
}
