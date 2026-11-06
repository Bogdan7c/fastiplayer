// Когда на документ встал наш скрипт MAIN-мира (сессия 10d).
//
// Обычно это `document_start`: скрипт раньше страницы, хук видит каждый кусок потока. Но Firefox при
// установке и обновлении экстеншена сам вставляет скрипты в уже открытые вкладки. Они встают посреди
// страницы: MediaSource сайта уже создан без нас, и его начало потеряно навсегда (разведка 10d).

/**
 * - `at_document_start` — скрипт встал раньше страницы;
 * - `after_page_started` — браузер вставил скрипт в уже загруженный документ.
 */
export type ScriptArrival = "at_document_start" | "after_page_started";

/**
 * Определить по состоянию документа на старте скрипта. Чистая функция.
 * Документы `about:` исключены: у пустого `about:blank` `readyState` бывает `complete` и на обычном
 * `document_start`, а передавать в нём нечего, кроме видео, которое страница строит на наших глазах.
 */
export function scriptArrivalOf(documentReadyState: string, documentUrl: string): ScriptArrival {
  if (documentUrl.startsWith("about:")) return "at_document_start";
  return documentReadyState === "loading" ? "at_document_start" : "after_page_started";
}
