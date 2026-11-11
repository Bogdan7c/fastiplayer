// Заголовок вкладки сеанса → плееру `page_title_changed` (протокол 1.9, сессия 16a): на одностраничном
// сайте серия сменилась без нового документа, и окно плеера с Info должны показать новое название.
//
// Правила (владелец — этот модуль):
// - чистка и предел — те же, что у заголовка при открытии сеанса (`pageTitleChangedMessage` →
//   `sanitizePageTitle`), поэтому сравнивается и шлётся уже ОЧИЩЕННЫЙ заголовок;
// - повтор не шлётся: заголовок из `open_session` считается уже отправленным, а «последним отправленным»
//   становится только то, что действительно ушло в очередь сокета;
// - плееру старше 1.9 слово не шлётся: его строгий разбор отверг бы всё сообщение;
// - пока плеер не слушает страницу (связь ещё не принята, страница ушла сиротой `page_left` или
//   возвращается в новую вкладку и `page_returned` ещё не ушло) — не шлётся: для плеера это нарушение.

import { pageSessionIntroducedIn, pageTitleChangedMessage, versionIncludes, type PageSessionMessage, type ProtocolVersion } from "../../../protocol/index.ts";

/** Что сделал модуль с новым заголовком (для тестов и журнала разработчика). */
export type PageTitleOutcome =
  /** Ушёл плееру. */
  | "sent"
  /** После чистки совпал с последним отправленным. */
  | "same_title"
  /** Плеер договорился на версию до 1.9: слова не знает. */
  | "player_too_old"
  /** Плеер сейчас страницу не слушает (до `attached`, сирота, возврат страницы). */
  | "player_not_listening";

export interface PageTitleRelayPorts {
  /** Заголовок, который ушёл плееру в `open_session` (уже очищен `sanitizePageTitle`). */
  readonly openedTitle: string;
  readonly agreedProtocol: () => ProtocolVersion;
  /** Слушает ли плеер сейчас слова страницы (решает сеанс: фаза, связь, возврат страницы). */
  readonly playerHearsPage: () => boolean;
  /** Положить сообщение в очередь сокета сеанса. */
  readonly send: (message: PageSessionMessage) => void;
}

export interface PageTitleRelay {
  /** Браузер сообщил новый заголовок вкладки сеанса (сырой, как его видит браузер). */
  titleSeen(rawTitle: string): PageTitleOutcome;
}

export function createPageTitleRelay(ports: PageTitleRelayPorts): PageTitleRelay {
  let lastSentTitle = ports.openedTitle;
  return {
    titleSeen(rawTitle) {
      const message = pageTitleChangedMessage(rawTitle);
      if (message.title === lastSentTitle) return "same_title";
      if (!versionIncludes(ports.agreedProtocol(), pageSessionIntroducedIn("page_title_changed"))) return "player_too_old";
      if (!ports.playerHearsPage()) return "player_not_listening";
      ports.send(message);
      lastSentTitle = message.title;
      return "sent";
    },
  };
}
