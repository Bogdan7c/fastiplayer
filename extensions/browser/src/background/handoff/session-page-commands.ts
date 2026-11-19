// Готовые команды странице и закрытие порта фрейма: сеанс зовёт их одной строкой из разных концов.

import type { PageCommand } from "../../shared/handoff-contract.ts";
import type { FramePort } from "./frame-port.ts";
import type { PreviousVideoLeave } from "./video-relocation.ts";

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

/**
 * Снять тень прежнего ролика ленты при шаге (16b3b, решение 372): позиция ТЕНИ — та секунда, где страница
 * остановила ролик, — и пауза. Вернётся человек к живому элементу — сайт продолжит с неё, как без моста.
 */
export const LEAVE_PREVIOUS_FEED_REEL: Extract<PageCommand, { kind: "end_handoff" }> = {
  kind: "end_handoff",
  position: { kind: "shadow_position" },
  resume: { kind: "exactly", resume: "paused" },
};

/** Команда снятия прежнего видео при переезде по решению переезда (`PreviousVideoLeave`). */
export function leavePreviousVideoCommand(leave: PreviousVideoLeave): Extract<PageCommand, { kind: "end_handoff" }> {
  switch (leave) {
    case "at_its_second":
      return LEAVE_PREVIOUS_FEED_REEL;
    case "at_start":
      return LEAVE_PREVIOUS_VIDEO;
  }
}

/**
 * Вернуть видео странице по кнопке «Return» заглушки (решение 236): позиция ТЕНИ (линия времени страницы,
 * а не наша — часы плеера туда уже привели тень), страница играет дальше.
 */
export const RETURN_PLAYING_AT_SHADOW: Extract<PageCommand, { kind: "end_handoff" }> = {
  kind: "end_handoff",
  position: { kind: "shadow_position" },
  resume: { kind: "exactly", resume: "playing" },
};

/**
 * Зритель закрыл окно плеера без прощального `return_to_page` (решение 238 → решение 51): видео остаётся на
 * позиции тени и НА ПАУЗЕ — иначе страница заиграла бы сама за спиной человека, который только что закрыл окно.
 */
export const RETURN_PAUSED_AT_SHADOW: Extract<PageCommand, { kind: "end_handoff" }> = {
  kind: "end_handoff",
  position: { kind: "shadow_position" },
  resume: { kind: "exactly", resume: "paused" },
};

export function disconnectQuietly(port: FramePort): void {
  try {
    port.disconnect();
  } catch {
    // Порт уже закрыт браузером — освобождать нечего.
  }
}
