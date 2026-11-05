// Готовые команды странице и закрытие порта фрейма: сеанс зовёт их одной строкой из разных концов.

import type { PageCommand } from "../../shared/handoff-contract.ts";
import type { FramePort } from "./frame-port.ts";

/** Снять тень без указаний плеера: позиция тени, играть/стоять как тень. */
export const END_WITHOUT_PLAYER: Extract<PageCommand, { kind: "end_handoff" }> = {
  kind: "end_handoff",
  position: { kind: "shadow_position" },
  resume: { kind: "as_shadow" },
};

/** Снять тень прежнего ролика при переезде: начало файла и пауза, второго `ended` страница не получит. */
export const LEAVE_PREVIOUS_VIDEO: Extract<PageCommand, { kind: "end_handoff" }> = {
  kind: "end_handoff",
  position: { kind: "at", position_us: 0 },
  resume: { kind: "exactly", resume: "paused" },
};

export function disconnectQuietly(port: FramePort): void {
  try {
    port.disconnect();
  } catch {
    // Порт уже закрыт браузером — освобождать нечего.
  }
}
