// Каким концом кончается сеанс, когда видео пропало и нового нет (сессия 16a, решения 179, 307, 42).
// Чистая функция без состояния: её зовут и правило `handoff_ended` от страницы (session-end-rules.ts), и
// истечение ожидания переезда (handoff-session.ts).
//
// Таблица:
// | что пропало                              | адрес вкладки с начала видео | конец                             |
// |------------------------------------------|------------------------------|-----------------------------------|
// | видео посреди ролика (пусто > 5 с, чужой | не сменился                  | 4009 «The page took the video     |
// | источник, переезд не пришёл за 15 с)     |                              | back»                             |
// | серия доиграла, следующая не пришла за   | не сменился                  | 4011 «The page didn't start the   |
// | 60 с                                     |                              | next episode» (старому плееру 4009)|
// | любое из двух                            | сменился                     | `tab_gone{navigated_away}`:       |
// |                                          |                              | досмотр скачанного, «Source tab   |
// |                                          |                              | closed» (решение 42)              |
//
// Почему при смене адреса даже после конца серии — уход, а не 4011: одностраничник ушёл со страницы
// сериала (на главную, в поиск), и «страница не поставила следующую серию» было бы неправдой.

import { closeReasonForUnstartedNextEpisode, type ProtocolVersion } from "../../../protocol/index.ts";
import type { HandoffSessionEnd } from "../session-contract.ts";
import type { QueueDeliveringCloseReason } from "../session-socket.ts";
import type { NextVideoWait } from "../video-relocation.ts";
import type { PageAddressSinceVideoBegan } from "./page-address-tracker.ts";

/** Что пропало. */
export type SourceLoss =
  /** Видео ушло посреди ролика: страница его забрала (если адрес тот же). */
  | "video_taken_back"
  /** Серия доиграла у плеера, а следующую страница не поставила в срок. */
  | "next_episode_not_started";

/** Конец сеанса по потере источника. */
export type SourceLossEnd =
  /** Одностраничник ушёл без видео (решение 42): `tab_gone{navigated_away}` после очереди. */
  | { readonly kind: "page_navigated_away" }
  /** Вкладка на месте: дописать очередь и закрыть кодом причины. */
  | { readonly kind: "end_on_page_side"; readonly closeReason: QueueDeliveringCloseReason; readonly end: HandoffSessionEnd };

/** Что сеанс знает в момент потери. */
export interface SourceLossFacts {
  readonly pageAddress: PageAddressSinceVideoBegan;
  readonly agreedProtocol: ProtocolVersion;
}

/** Какая потеря стоит за истёкшим ожиданием переезда. */
export function sourceLossOfExpiredWait(wait: NextVideoWait): SourceLoss {
  return wait === "after_episode_end" ? "next_episode_not_started" : "video_taken_back";
}

export function endAfterSourceLoss(loss: SourceLoss, facts: SourceLossFacts): SourceLossEnd {
  if (facts.pageAddress === "changed") return { kind: "page_navigated_away" };
  if (loss === "video_taken_back") return { kind: "end_on_page_side", closeReason: "page_took_video_back", end: { kind: "page_took_video_back" } };
  return { kind: "end_on_page_side", closeReason: closeReasonForUnstartedNextEpisode(facts.agreedProtocol), end: { kind: "next_episode_not_started" } };
}
