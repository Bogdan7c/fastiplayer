// Хук XMLHttpRequest: ловим документы субтитров, которые плеер сайта качает через XHR (сессия 13b2).
//
// Когда ставится слушатель. Ответ XHR целиком лежит в самом объекте, поэтому клонировать нечего — но
// страничный обработчик `load` может забрать буфер (`postMessage` с transfer отсоединяет `ArrayBuffer`).
// Чтобы успеть раньше, слушатель `load` ставится в `open()` (до любых слушателей, которые страница
// добавит после него), а не в `send()`: слушатели срабатывают в порядке подписки.
//
// Что читаем, по `responseType`:
//  - `""`/`"text"` — `responseText` → байты UTF-8 (чужие кодировки текст уже перекодировал браузер);
//  - `"json"` — объект → `JSON.stringify`, и только если адрес явно субтитровый (`fmt=json3`, `timedtext`, …);
//  - `"arraybuffer"` — собственная копия байт;
//  - `"blob"`, `"document"` — не читаем: для них нужен асинхронный разбор, а настоящие плееры субтитров их не используют.
// Фильтр (`decideSubtitleResponse`) и проверка содержимым — те же, что у `fetch`.

import type { ChannelOriginals } from "../page-channel.ts";
import { wrapMethod, type OddityReporter } from "../prototype-wrap.ts";
import type { ByteTools } from "./byte-tools.ts";
import type { SubtitleDownloadIntake } from "./download-intake.ts";
import { addressLooksLikeSubtitles, decideSubtitleResponse, MAX_SUBTITLE_RESPONSE_BYTES } from "./response-filter.ts";
import type { XhrReaders } from "./xhr-readers.ts";

export interface XhrHookTools {
  readonly readers: XhrReaders;
  readonly channel: Pick<ChannelOriginals, "addListener">;
  readonly bytes: ByteTools;
  readonly intake: SubtitleDownloadIntake;
  readonly reportOddity: OddityReporter;
}

/** Байты ответа по его `responseType`; `null` — этот вид не читаем или в нём нет документа. */
function bodyBytesOf(xhr: object, address: string, tools: XhrHookTools): Uint8Array | null {
  const { readers, bytes } = tools;
  switch (readers.responseType(xhr)) {
    case "":
    case "text": {
      const text = readers.responseText(xhr);
      // В UTF-16 каждая единица — минимум один байт UTF-8: текст длиннее потолка в единицах заведомо длиннее в байтах.
      if (text.length === 0 || text.length > MAX_SUBTITLE_RESPONSE_BYTES || bytes.encodeUtf8 === null) return null;
      return bytes.encodeUtf8(text);
    }
    case "json": {
      // JSON читаем только по явно субтитровому адресу: иначе любой API-ответ сайта попал бы в обработку.
      if (!addressLooksLikeSubtitles(address) || bytes.stringifyJson === null) return null;
      const text = bytes.stringifyJson(readers.responseValue(xhr));
      return text === null || text.length === 0 || text.length > MAX_SUBTITLE_RESPONSE_BYTES || bytes.encodeUtf8 === null ? null : bytes.encodeUtf8(text);
    }
    case "arraybuffer":
      return bytes.copyOf(readers.responseValue(xhr));
    default:
      return null;
  }
}

/** Запрос завершён успешно (`load`): фильтр, байты, приём. `openedAddress` — адрес из `open()`, если `responseURL` пуст. */
function onXhrLoad(xhr: object, openedAddress: string, tools: XhrHookTools): void {
  const { readers, bytes } = tools;
  const responseUrl = readers.responseUrl(xhr);
  const address = responseUrl === "" ? openedAddress : responseUrl;
  const verdict = decideSubtitleResponse({
    url: address,
    status: readers.status(xhr),
    responseType: null,
    contentType: readers.header(xhr, "content-type"),
    contentLength: readers.header(xhr, "content-length"),
  });
  if (!verdict.read) return;
  const body = bodyBytesOf(xhr, address, tools);
  // Размер проверяем и после чтения: заголовка могло не быть, а `arraybuffer` бывает любой длины.
  if (body === null || bytes.lengthOf(body) === 0 || bytes.lengthOf(body) > MAX_SUBTITLE_RESPONSE_BYTES) return;
  tools.intake.offer(address, body);
}

/** Поставить хук на `XMLHttpRequest.prototype.open`. `false` — метода нет, ничего не тронуто. */
export function installXhrSubtitleHook(tools: XhrHookTools): boolean {
  /** Адрес из последнего `open()` этого запроса. */
  const openedAddresses = new WeakMap<object, string>();
  /** Запросы, на которые слушатель уже поставлен (`open()` может звать один объект много раз). */
  const listened = new WeakSet<object>();
  return wrapMethod(tools.readers.prototype, "open", tools.reportOddity, (target, args) => {
    if (typeof target !== "object" || target === null) return;
    const rawAddress: unknown = args[1];
    openedAddresses.set(target, typeof rawAddress === "string" ? rawAddress : "");
    if (listened.has(target)) return;
    listened.add(target);
    tools.channel.addListener(
      target,
      "load",
      () => {
        try {
          onXhrLoad(target, openedAddresses.get(target) ?? "", tools);
        } catch {
          tools.reportOddity("observing an XHR response failed");
        }
      },
      false,
    );
  });
}
