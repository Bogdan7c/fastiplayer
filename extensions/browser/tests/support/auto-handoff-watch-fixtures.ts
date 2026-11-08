// Заготовки охраны режима 2 для тестов (решения 260–261).
import type { AutoHandoffWatch } from "../../src/shared/auto-handoff-contract.ts";

export { AUTO_HANDOFF_WATCH_OFF } from "../../src/shared/auto-handoff-contract.ts";

/** Охрана включена, общие фильтры звука и цикла включены: беззвучное и зацикленное охрана пропускает сама. */
export const WATCH_ON: AutoHandoffWatch = { state: "on", muted: "skip", looped: "skip" };

/** Охрана включена, общие фильтры выключены (значение по умолчанию): охрана задерживает всё, судит фон. */
export const WATCH_ON_UNFILTERED: AutoHandoffWatch = { state: "on", muted: "hold", looped: "hold" };
