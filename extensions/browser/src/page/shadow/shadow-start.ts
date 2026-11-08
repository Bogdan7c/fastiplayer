// Запуск тени на элементе: обычный (передача) и задержанный (сторож следующего видео, сессия 09c2).
// Контроллер один (`shadow-session.ts`), разница только в том, когда снимается файл и что доступно снаружи.

import type { HandoffEndPosition, HandoffEndResume, PageEvent, ShadowCommand } from "../../shared/handoff-contract.ts";
import type { HeldShadowPlayback, PendingPlay, ShadowSourceKind } from "../handoff-seam.ts";
import { interruptFileReturn } from "./file-shadow-lifecycle.ts";
import { installShadowOverrides } from "./prototype-overrides.ts";
import { ShadowController, type FileStopMoment, type ShadowEnvironment } from "./shadow-session.ts";

/** Управление тенью для MAIN-мира. */
export interface ShadowSession {
  apply(command: ShadowCommand): void;
  /** Снять тень по команде `end_handoff`; шлёт `handoff_ended{returned}`. Повтор — ничего. */
  end(position: HandoffEndPosition, resume: HandoffEndResume): void;
}

/** Тень на задержанном видео (сессия 09c2): решение фона ещё впереди. */
export interface HeldShadowSession extends ShadowSession {
  commitHold(): void;
  pageSawPlaying(): boolean;
  takeOverPendingPlays(plays: readonly PendingPlay[]): void;
  showHeldPagePlayback(action: "play" | "pause"): void;
}

/**
 * Ставит элемент в тень. `emit` — события наружу (page_seek, player_seek_applied, page_playback,
 * handoff_ended). `source` — что передаётся: MSE или файл `<video src>` (handoff-seam.ts).
 */
export function startShadow(
  element: HTMLMediaElement,
  emit: (event: PageEvent) => void,
  env: ShadowEnvironment,
  source: ShadowSourceKind,
): ShadowSession {
  const controller = createController(element, emit, env, source, "stop_file_now", "as_real_element");
  return {
    apply: (command) => controller.apply(command),
    end: (position, resume) => controller.end(position, resume),
  };
}

/**
 * Тень сторожа следующего видео (сессия 09c2): элемент сразу стоит и молчит, но файл не снимается,
 * пока фон не скажет «берём» (`commitHold`). Отказ тогда стоит дёшево: элемент играет дальше сам.
 * `initialPlayback` — с чего начинает модель: при возврате страницы (11b2) страница уже просила играть,
 * а настоящий `play()` сторож не звал.
 */
export function startHeldShadow(
  element: HTMLMediaElement,
  emit: (event: PageEvent) => void,
  env: ShadowEnvironment,
  source: ShadowSourceKind,
  initialPlayback: HeldShadowPlayback,
): HeldShadowSession {
  const controller = createController(element, emit, env, source, "keep_file_until_commit", initialPlayback);
  return {
    apply: (command) => controller.apply(command),
    end: (position, resume) => controller.end(position, resume),
    commitHold: () => controller.commitHold(),
    pageSawPlaying: () => controller.pageSawPlaying(),
    takeOverPendingPlays: (plays) => controller.takeOverPendingPlays(plays),
    showHeldPagePlayback: (action) => controller.showHeldPagePlayback(action),
  };
}

function createController(
  element: HTMLMediaElement,
  emit: (event: PageEvent) => void,
  env: ShadowEnvironment,
  source: ShadowSourceKind,
  fileStop: FileStopMoment,
  initialPlayback: HeldShadowPlayback,
): ShadowController {
  const registry = installShadowOverrides(env.originals);
  // Прежняя тень файла ещё возвращает элемент (находка 6 ревью 09b): закончить её возврат сейчас.
  const interruptedFile = interruptFileReturn(element);
  if (registry.isShadowed(element)) throw new Error("shadow: element is already shadowed");
  return new ShadowController(element, emit, env, registry, source, interruptedFile, fileStop, initialPlayback);
}
