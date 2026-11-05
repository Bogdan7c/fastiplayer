// Строгий разбор сообщений слежения за рекламой: что пропускаем, что отбрасываем (лимиты протокола — на краю).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { AD_WATCH_REPORT_KIND, ARM_AD_WATCH_KIND, MAX_AD_WATCH_LEASE_MS, PRESS_AD_SKIP_KIND } from "../../src/shared/ad-watch-contract.ts";
import { parseAdWatchReport, parseArmAdWatchRequest, parsePageAdState, parsePressAdSkipRequest, parsePressSkipOutcome } from "../../src/shared/ad-watch-validation.ts";

const report = (ad: unknown): unknown => ({ kind: AD_WATCH_REPORT_KIND, ad });
const showing = (pod: unknown, skip: unknown): unknown => ({ kind: "showing", pod, skip });

describe("донесение о рекламе", () => {
  it("все формы протокола проходят", () => {
    const valid = [
      { kind: "none" },
      showing({ kind: "unknown" }, { kind: "unavailable" }),
      showing({ kind: "known", index: 1, count: 1 }, { kind: "available" }),
      showing({ kind: "known", index: 64, count: 64 }, { kind: "on_page" }),
      showing({ kind: "unknown" }, { kind: "countdown", seconds: 0 }),
      showing({ kind: "unknown" }, { kind: "countdown", seconds: 3_600 }),
    ];
    for (const ad of valid) assert.deepEqual(parseAdWatchReport(report(ad)), { kind: AD_WATCH_REPORT_KIND, ad });
  });

  it("всё за лимитами протокола отбрасывается: на провод такое пойти не может", () => {
    const invalid = [
      showing({ kind: "known", index: 3, count: 2 }, { kind: "unavailable" }),
      showing({ kind: "known", index: 0, count: 2 }, { kind: "unavailable" }),
      showing({ kind: "known", index: 1, count: 65 }, { kind: "unavailable" }),
      showing({ kind: "known", index: 1.5, count: 2 }, { kind: "unavailable" }),
      showing({ kind: "unknown" }, { kind: "countdown", seconds: 3_601 }),
      showing({ kind: "unknown" }, { kind: "countdown", seconds: -1 }),
      showing({ kind: "unknown" }, { kind: "countdown", seconds: "5" }),
      showing({ kind: "unknown" }, { kind: "pressed" }),
      showing({ kind: "unknown" }, { kind: "available", extra: 1 }),
      { kind: "showing", pod: { kind: "unknown" } },
      { kind: "none", text: "Skip ad" },
      { kind: "maybe" },
      "none",
      null,
      [],
    ];
    for (const ad of invalid) assert.equal(parseAdWatchReport(report(ad)), undefined, JSON.stringify(ad));
  });

  it("лишние и чужие поля самого донесения отбрасываются", () => {
    assert.equal(parseAdWatchReport({ kind: AD_WATCH_REPORT_KIND, ad: { kind: "none" }, tab: 5 }), undefined);
    assert.equal(parseAdWatchReport({ kind: "other", ad: { kind: "none" } }), undefined);
    assert.equal(parseAdWatchReport(undefined), undefined);
    assert.equal(parsePageAdState({ kind: "none" })?.kind, "none");
  });
});

describe("просьбы фона", () => {
  it("аренда: целые миллисекунды в пределах", () => {
    assert.deepEqual(parseArmAdWatchRequest({ kind: ARM_AD_WATCH_KIND, lease_ms: 3_000 }), { kind: ARM_AD_WATCH_KIND, lease_ms: 3_000 });
    for (const lease of [0, -5, 1.5, MAX_AD_WATCH_LEASE_MS + 1, "3000", Number.NaN]) {
      assert.equal(parseArmAdWatchRequest({ kind: ARM_AD_WATCH_KIND, lease_ms: lease }), undefined, String(lease));
    }
    assert.equal(parseArmAdWatchRequest({ kind: ARM_AD_WATCH_KIND, lease_ms: 3_000, x: 1 }), undefined);
  });

  it("нажатие: только ровно {kind}", () => {
    assert.deepEqual(parsePressAdSkipRequest({ kind: PRESS_AD_SKIP_KIND }), { kind: PRESS_AD_SKIP_KIND });
    assert.equal(parsePressAdSkipRequest({ kind: PRESS_AD_SKIP_KIND, selector: ".x" }), undefined);
    assert.equal(parsePressAdSkipRequest({ kind: "arm_ad_watch" }), undefined);
  });

  it("итог нажатия — закрытый набор", () => {
    for (const outcome of ["pressed", "no_button", "person_only"]) assert.equal(parsePressSkipOutcome(outcome), outcome);
    for (const bad of ["Pressed", "ok", 1, null, undefined, { kind: "pressed" }]) assert.equal(parsePressSkipOutcome(bad), undefined);
  });
});
