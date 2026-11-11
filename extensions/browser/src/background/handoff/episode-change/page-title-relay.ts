// Заголовок вкладки сеанса → плееру `page_title_changed` (протокол 1.9, сессия 16a): на одностраничном
// сайте серия сменилась без нового документа, и окно плеера с Info должны показать новое название.
//
// Правила (владелец — этот модуль):
// - чистка и предел — те же, что у заголовка при открытии сеанса (`pageTitleChangedMessage` →
//   `sanitizePageTitle`), поэтому сравнивается и шлётся уже ОЧИЩЕННЫЙ заголовок;
// - повтор не шлётся: заголовок из `open_session` считается уже отправленным, а «последним отправленным»
//   становится только то, что действительно ушло в очередь сокета;
// - не чаще одного слова за `PAGE_TITLE_MIN_INTERVAL_MS` на сеанс; страница недоверенная (инвариант 1), и
//   мигающий или «бегущий» заголовок иначе заставлял бы плеер писать строку журнала, менять заголовок окна и
//   публиковать MPRIS десятки раз в секунду. Первое изменение после тишины уходит сразу; изменения внутри
//   интервала склеиваются, и по его концу уходит ПОСЛЕДНЕЕ (замыкающая отправка), если оно не равно уже
//   отправленному. Замыкающая отправка отбрасывается, если сеанс кончен (`stop`) или плеер страницу не слушает;
// - плееру старше 1.9 слово не шлётся: его строгий разбор отверг бы всё сообщение;
// - пока плеер не слушает страницу (связь ещё не принята, страница ушла сиротой `page_left` или
//   возвращается в новую вкладку и `page_returned` ещё не ушло) — не шлётся: для плеера это нарушение.

import type { Scheduler } from "../../host-link/native-port.ts";
import { createTimerSlot } from "../../host-link/timer-slot.ts";
import { pageSessionIntroducedIn, pageTitleChangedMessage, versionIncludes, type PageSessionMessage, type ProtocolVersion } from "../../../protocol/index.ts";

/**
 * Минимальный промежуток между двумя `page_title_changed` одного сеанса. 2 с: настоящая смена серии
 * случается раз в десятки минут, а смену заголовка человек замечает с задержкой в секунду-другую, так что
 * задержка до 2 с незаметна; при этом анимированный заголовок (бегущая строка, «(3) new messages») даёт
 * плееру не больше 0,5 слова в секунду вместо десятков.
 */
export const PAGE_TITLE_MIN_INTERVAL_MS = 2_000;

/** Что сделал модуль с новым заголовком (для тестов и журнала разработчика). */
export type PageTitleOutcome =
  /** Ушёл плееру. */
  | "sent"
  /** После чистки совпал с последним отправленным. */
  | "same_title"
  /** Плеер договорился на версию до 1.9: слова не знает. */
  | "player_too_old"
  /** Плеер сейчас страницу не слушает (до `attached`, сирота, возврат страницы). */
  | "player_not_listening"
  /** Слишком рано после прошлой отправки: заголовок ждёт конца интервала (уйдёт последний из ждущих). */
  | "deferred";

export interface PageTitleRelayPorts {
  /** Часы и таймеры фона (в тестах — фейковые). */
  readonly scheduler: Scheduler;
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
  /** Сеанс кончен: ждущая замыкающая отправка снимается и уже не сработает. Повторный вызов безвреден. */
  stop(): void;
}

export function createPageTitleRelay(ports: PageTitleRelayPorts): PageTitleRelay {
  const trailingTimer = createTimerSlot(ports.scheduler);
  let lastSentTitle = ports.openedTitle;
  // Когда ушло последнее слово (`undefined` — ещё не уходило: первое уходит сразу).
  let lastSentAtMs: number | undefined;
  // Последний заголовок, придержанный внутри интервала (самый свежий выигрывает).
  let pendingTitle: string | undefined;

  const dropPending = (): void => {
    pendingTitle = undefined;
    trailingTimer.cancel();
  };

  const sendNow = (title: string): void => {
    ports.send({ type: "page_title_changed", title });
    lastSentTitle = title;
    lastSentAtMs = ports.scheduler.now();
  };

  // Конец интервала: уходит последний придержанный, если плеер слушает и он отличается от отправленного.
  const sendTrailing = (): void => {
    const title = pendingTitle;
    pendingTitle = undefined;
    if (title === undefined || title === lastSentTitle || !ports.playerHearsPage()) return;
    sendNow(title);
  };

  return {
    titleSeen(rawTitle) {
      const message = pageTitleChangedMessage(rawTitle);
      if (!versionIncludes(ports.agreedProtocol(), pageSessionIntroducedIn("page_title_changed"))) return "player_too_old";
      if (!ports.playerHearsPage()) {
        dropPending();
        return message.title === lastSentTitle ? "same_title" : "player_not_listening";
      }
      if (message.title === lastSentTitle) {
        // Заголовок вернулся к уже отправленному: придержанный промежуточный больше не нужен.
        dropPending();
        return "same_title";
      }
      const sinceLastSendMs = lastSentAtMs === undefined ? Infinity : ports.scheduler.now() - lastSentAtMs;
      if (sinceLastSendMs >= PAGE_TITLE_MIN_INTERVAL_MS) {
        dropPending();
        sendNow(message.title);
        return "sent";
      }
      pendingTitle = message.title;
      // Таймер заводится один раз на интервал; следующие изменения лишь подменяют `pendingTitle`.
      if (!trailingTimer.isActive()) trailingTimer.start(PAGE_TITLE_MIN_INTERVAL_MS - sinceLastSendMs, sendTrailing);
      return "deferred";
    },
    stop: dropPending,
  };
}
