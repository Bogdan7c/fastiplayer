// Команда тени `live_window` (сессия 10a3) на входе MAIN-мира: строгая форма и договор окна
// `start_us <= end_us <= edge_us` — как при разборе протокола, мусор молча отбрасывается.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { parseIncomingPageMessage } from "../../src/page/page-command-guard.ts";

const shadow = (command: unknown) => ({ kind: "shadow", command });
const range = { kind: "range", start_us: 40_000_000, end_us: 60_500_000 };

describe("MAIN-мир: команда тени live_window", () => {
  it("принимает окно с DVR и эфир без DVR", () => {
    const withRange = shadow({ kind: "live_window", edge_us: 61_000_000, seekable: range });
    assert.deepEqual(parseIncomingPageMessage(withRange), withRange);
    const withoutDvr = shadow({ kind: "live_window", edge_us: 61_000_000, seekable: { kind: "none" } });
    assert.deepEqual(parseIncomingPageMessage(withoutDvr), withoutDvr);
    const onePoint = shadow({ kind: "live_window", edge_us: 5, seekable: { kind: "range", start_us: 5, end_us: 5 } });
    assert.deepEqual(parseIncomingPageMessage(onePoint), onePoint, "окно в одну точку допустимо");
  });

  it("отвергает вывернутое окно, конец за краем, лишние поля, не те числа и поколение (его сверил фон)", () => {
    const rejected: unknown[] = [
      { kind: "live_window", edge_us: 61_000_000, seekable: { kind: "range", start_us: 50, end_us: 40 } },
      { kind: "live_window", edge_us: 60_000_000, seekable: range },
      { kind: "live_window", edge_us: 61_000_000, seekable: range, generation: 3 },
      { kind: "live_window", edge_us: -1, seekable: { kind: "none" } },
      { kind: "live_window", edge_us: 1.5, seekable: { kind: "none" } },
      { kind: "live_window", edge_us: 61_000_000, seekable: { kind: "none", start_us: 0 } },
      { kind: "live_window", edge_us: 61_000_000, seekable: { kind: "dvr" } },
      { kind: "live_window", edge_us: 61_000_000 },
      { kind: "live_window", edge_us: 61_000_000, seekable: null },
    ];
    for (const command of rejected) assert.equal(parseIncomingPageMessage(shadow(command)), null, JSON.stringify(command));
  });
});
