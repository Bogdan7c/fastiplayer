import test from "node:test";
import assert from "node:assert/strict";
import { activationLib, base64Lib } from "./support.mjs";

const fakeWindow = (search, topSearch) => {
  const win = { location: { search } };
  win.top = topSearch === undefined ? win : { get location() { if (topSearch === "throw") throw new Error("SecurityError"); return { search: topSearch }; } };
  return win;
};

test("handoff and run come from the frame URL or the top URL", () => {
  assert.deepEqual(activationLib.resolveActivation(fakeWindow("?run=a-1&handoff=1")), { handoff: true, run: "a-1" });
  assert.deepEqual(activationLib.resolveActivation(fakeWindow("?run=a-1")), { handoff: false, run: "a-1" });
  assert.deepEqual(activationLib.resolveActivation(fakeWindow("", "?run=top-1&handoff=1")), { handoff: true, run: "top-1" });
});

test("a cross-origin top page does not break activation and invalid run ids are rejected", () => {
  assert.deepEqual(activationLib.resolveActivation(fakeWindow("?handoff=1&run=ok", "throw")), { handoff: true, run: "ok" });
  assert.equal(activationLib.resolveActivation(fakeWindow("?handoff=0", "throw")).handoff, false);
  assert.equal(activationLib.parseBenchParams("?run=BAD%20RUN&handoff=1").run, null);
});

test("base64 round trip preserves arbitrary bytes, including > 32 KiB", () => {
  const bytes = new Uint8Array(100000).map((_, index) => (index * 31) % 256);
  assert.deepEqual(base64Lib.base64ToBytes(base64Lib.bytesToBase64(bytes)), bytes);
});
