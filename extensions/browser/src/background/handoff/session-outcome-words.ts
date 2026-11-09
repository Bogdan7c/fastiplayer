// Машинные слова исходов сеанса: отказ `open_session` и конец сокета в строках `reason` для попапа 10.

import type { GrantOutcome } from "../../protocol/index.ts";
import type { HandOffTabOutcome, HandoffStartOutcome, PlayerWindow } from "../../shared/handoff-contract.ts";
import type { OpenSessionOutcome } from "../host-link/open-session-requests.ts";
import type { SocketEndCause } from "./session-socket.ts";

/** Отказ `open_session` → исход для попапа 10 (причина — машинная строка). */
export function describeRefusal(outcome: Exclude<OpenSessionOutcome, { kind: "granted" }>): HandOffTabOutcome {
  switch (outcome.kind) {
    case "refused":
      return { kind: "player_unavailable", reason: outcome.refusal.reason.kind };
    case "no_reply":
      return { kind: "player_unavailable", reason: "no_reply" };
    case "not_connected": {
      const status = outcome.status;
      return { kind: "player_unavailable", reason: status.state === "player_unavailable" ? status.reason : status.state };
    }
    case "invalid_request":
      return { kind: "failed", reason: `invalid_open_session:${outcome.error.kind}` };
  }
}

/** Ответ плеера на `open_session` -> окно плеера для зрителя (решение 216): только `started` — новое окно. */
export function playerWindowOf(grantOutcome: GrantOutcome): PlayerWindow {
  return grantOutcome === "started" ? "new" : "already_open";
}

export function describeSocketEnd(cause: SocketEndCause): string {
  switch (cause.kind) {
    case "player_closed":
      return cause.reason ?? `code_${cause.code}`;
    case "player_protocol_violation":
    case "page_message_rejected":
      return `${cause.kind}:${cause.detail}`;
    default:
      return cause.kind;
  }
}

/** Исход команды по отказу страницы начать тень (вынесено из `handoff-session.ts` без изменения поведения). */
export function outcomeOfRefusedBegin(refused: Exclude<HandoffStartOutcome, "started">): HandOffTabOutcome {
  return refused === "protected" ? { kind: "protected" } : { kind: "no_video" };
}
