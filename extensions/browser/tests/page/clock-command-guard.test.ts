// Команда тени `clock` на входе MAIN-мира (сессия 16a2): пометка «чьи это часы» обязательна и строго из договора
// (`CLOCK_RELATIONS`), мусор молча отбрасывается. Без пометки тень не отличила бы конец прошлого видео от своего.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { parseIncomingPageMessage } from "../../src/page/page-command-guard.ts";
import { CLOCK_RELATIONS } from "../../src/shared/handoff-contract.ts";

const shadow = (command: unknown) => ({ kind: "shadow", command });
const tick = { kind: "clock", position_us: 11_100_000, state: "playing", rate: 1000 };

describe("MAIN-мир: команда тени clock с пометкой (16a2)", () => {
  it("принимает каждую пометку договора и отдаёт её тени как есть", () => {
    for (const relation of CLOCK_RELATIONS) {
      const command = shadow({ ...tick, relation });
      assert.deepEqual(parseIncomingPageMessage(command), command, relation);
    }
  });

  it("отвергает тик без пометки, чужую пометку, не ту форму и номер поколения (его сверил фон)", () => {
    const rejected: unknown[] = [
      tick,
      { ...tick, relation: "maybe" },
      { ...tick, relation: null },
      { ...tick, relation: 1 },
      { ...tick, relation: "THIS_VIDEO" },
      { ...tick, relation: "this_video", generation: 2 },
      { ...tick, state: "ended", relation: ["previous_video"] },
    ];
    for (const command of rejected) assert.equal(parseIncomingPageMessage(shadow(command)), null, JSON.stringify(command));
  });
});
