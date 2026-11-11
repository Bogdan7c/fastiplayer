// Договор сеанса передачи (сессия 09d вынесла его из `handoff-session.ts`, который упёрся в предел размера):
// чем кончается сеанс и что владелец сеанса может у него спросить. Только типы, поведения здесь нет.

import type { VariantWatchReport } from "../../shared/variant-watch-contract.ts";
import type { HandOffTabOutcome, HeldVideoReply, HeldVideoReport, PageEventBudgetKind, UnsupportedMediaForm, VideoRef } from "../../shared/handoff-contract.ts";
import type { PageAdState, PageReopenFailure, TabGoneReason } from "../../protocol/index.ts";
import type { PageStreamProblem } from "./page-event-translator.ts";
import type { SocketEndCause } from "./session-socket.ts";

/** Чем закончилась передача (для владельца и тестов; журнала в продукте нет). */
export type HandoffSessionEnd =
  | { readonly kind: "start_failed"; readonly outcome: HandOffTabOutcome }
  | { readonly kind: "page_gone"; readonly reason: TabGoneReason }
  /** Страница ушла сиротой, а вернуть её не вышло (11b2): `page_reopen_failed{reason}` и 4002. */
  | { readonly kind: "page_not_returned"; readonly reason: PageReopenFailure }
  /** Страница сама забрала видео, вкладка на месте (`handoff_ended{element_released}`, 4009). */
  | { readonly kind: "page_took_video_back" }
  /**
   * Серия доиграла, а следующую страница не поставила за `NEXT_EPISODE_WAIT_MS` (16a, решение 307): 4011 с
   * плеером 1.9+, со старым — 4009 (`closeReasonForUnstartedNextEpisode`).
   */
  | { readonly kind: "next_episode_not_started" }
  /** Страница сняла тень сама без нашей просьбы — сбой внутри страницы (4003, Д-6). */
  | { readonly kind: "page_side_failure" }
  /** Тень замолчала дольше предела пульса (4003, решение 180). */
  | { readonly kind: "shadow_silent" }
  /** Страница превысила бюджет событий или счётчик копий байтов (4003, решение 200); вид — только для журнала. */
  | { readonly kind: "page_flood"; readonly overBudget: PageEventBudgetKind | "forwarded_bytes" }
  /** Страница назвала адрес файла, который правило запрещает (4003, решение 199). */
  | { readonly kind: "local_address_refused" }
  /** Договорённая версия протокола не знает такого источника (HLS при протоколе 1.0): видео остаётся у страницы. */
  | { readonly kind: "source_needs_newer_player"; readonly form: UnsupportedMediaForm }
  | { readonly kind: "returned_to_page" }
  /**
   * Зритель нажал «Return» на заглушке поверх видео (решение 236): тень снята на её позиции, страница играет
   * дальше, сокет закрыт кодом 4010 сразу, без дописывания очереди и без `tab_gone`.
   */
  | { readonly kind: "returned_by_viewer" }
  | { readonly kind: "stream_rejected" }
  | { readonly kind: "player_ended"; readonly cause: SocketEndCause }
  | { readonly kind: "page_stream_problem"; readonly problem: PageStreamProblem };

/**
 * Шаг замены видео на той же вкладке (решение 215): у вкладки уже есть сеанс, и плеер, приняв новый,
 * закроет старый. Страница нового сеанса получает `begin_handoff` ТОЛЬКО после конца старого: иначе
 * контроллер страницы (`#beginHandoff`) закрыл бы живую старую передачу «как тень», и прежнее видео
 * заиграло бы вместо паузы на той же секунде. Сеанс вызывает шаг, когда плеер принял новый сеанс
 * (`attached`). Промис не отклоняется и ограничен по времени самим владельцем (не дольше
 * `HANDOFF_END_REPLY_TIMEOUT_MS`): зависший старый сеанс нового не держит.
 */
export type AfterPreviousSession = () => Promise<void>;

export interface HandoffSession {
  /** Исход старта: `started` или причина отказа. Всегда завершается. */
  readonly outcome: Promise<HandOffTabOutcome>;
  /** Браузер сообщил, что вкладка закрыта (`tabs.onRemoved`). */
  notifyTabRemoved(): void;
  /** Сторож задержал видео в этом фрейме: переехать на него или отпустить. */
  heldVideo(frameId: number, report: HeldVideoReport): HeldVideoReply;
  /** На экране плеера реклама (решение 204; сессия 09d читает только это). */
  adOnScreen(): boolean;
  /**
   * Элемент, на котором сеанс стоит сейчас (с учётом переезда 09c2); `null` — сеанс между видео, тень не
   * стоит ни на одном элементе. Нужен попапу (10b): видео вкладки уже в плеере или нет.
   */
  videoInPlayer(): VideoRef | null;
  /** Донесение фрейма о рекламе (сессия 09d); строгая форма проверена приёмником, вкладка — владельцем. */
  adWatchReport(frameId: number, ad: PageAdState): void;
  /** Донесение фрейма о меню вариантов страницы (14b); форма проверена приёмником, вкладка — владельцем. */
  variantWatchReport(frameId: number, report: VariantWatchReport): void;
}

