// Перевод сообщений плеера (уже строго разобранных `decodePlayerSession`) в действия сеанса.
// Чистая функция: решения о сокете, очереди и тени принимает владелец сеанса по её ответу.

import type { HandoffEndPosition, HandoffEndResume, ShadowCommand } from "../../shared/handoff-contract.ts";
import type { BufferIntake, PlayerSessionMessage, StreamGeneration } from "../../protocol/index.ts";
import type { RangeRequest } from "./range-fetcher.ts";

export type PlayerAction =
  /** Плеер принял `attach`: можно отдавать очередь. */
  | { readonly kind: "attached" }
  /** «Полон» придерживает рамки, «принимаю» — отпускает. */
  | { readonly kind: "buffer_intake"; readonly intake: BufferIntake }
  /** Команда тени странице. */
  | { readonly kind: "shadow"; readonly command: ShadowCommand }
  /** Снять тень (возврат или отказ поколения); после этого сеанс закрывается. */
  | {
      readonly kind: "end_handoff";
      readonly position: HandoffEndPosition;
      readonly resume: HandoffEndResume;
      readonly cause: "returned_to_page" | "stream_rejected";
    }
  /** Плеер просит байты файла `<video src>` (сессия 09b): их качает загрузчик диапазонов. */
  | { readonly kind: "fetch_range"; readonly request: RangeRequest }
  /** Плеер просит нажать кнопку пропуска рекламы (сессия 09d); решает сеанс: целевой фрейм знает только он. */
  | { readonly kind: "skip_ad" }
  /** Не относится к передаче (варианты — 14, следующая серия — 15) или к старому поколению — пропустить. */
  | { readonly kind: "ignore" };

/**
 * Что делать с сообщением плеера.
 * `latestGeneration` — последнее открытое поколение сеанса: отказ СТАРОГО поколения (хвост рекламы)
 * сеанс не рвёт — плеер уже бросил его и играет новое.
 */
export function translatePlayerMessage(message: PlayerSessionMessage, latestGeneration: StreamGeneration | undefined): PlayerAction {
  switch (message.type) {
    case "attached":
      return { kind: "attached" };
    case "buffer_state":
      return { kind: "buffer_intake", intake: message.intake };
    case "clock":
      // `seq` нужен только плееру для порядка; тень получает позицию, состояние и скорость.
      return { kind: "shadow", command: { kind: "clock", position_us: message.position_us, state: message.state, rate: message.rate } };
    case "play":
      return { kind: "shadow", command: { kind: "play" } };
    case "pause":
      return { kind: "shadow", command: { kind: "pause" } };
    case "set_rate":
      return { kind: "shadow", command: { kind: "set_rate", rate: message.rate } };
    case "seek":
      return { kind: "shadow", command: { kind: "seek", seek_id: message.seek_id, target_us: message.target_us } };
    case "return_to_page":
      return {
        kind: "end_handoff",
        position: { kind: "at", position_us: message.position_us },
        resume: { kind: "exactly", resume: message.resume },
        cause: "returned_to_page",
      };
    case "stream_rejected":
      if (message.generation !== latestGeneration) return { kind: "ignore" };
      return {
        kind: "end_handoff",
        position: { kind: "shadow_position" },
        resume: { kind: "as_shadow" },
        cause: "stream_rejected",
      };
    case "fetch_range":
      return { kind: "fetch_range", request: { request_id: message.request_id, offset: message.offset, length: message.length } };
    case "skip_ad":
      return { kind: "skip_ad" };
    case "select_variant":
    case "next_episode":
      // Меню вариантов — 14, следующая серия — 15: пока их не бывает.
      return { kind: "ignore" };
  }
}
