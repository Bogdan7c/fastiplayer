// Настоящий бандл хука воркера (тот, что сборка вшивает в page.js) в настоящем тексте blob-скрипта
// обёртки: выполняется в отдельном контексте `vm` поверх фейкового глобала воркера. Доказывает,
// что запечённая политика доходит до ответов про кодеки, хук стоит ДО кода сайта, рукопожатие
// не видно сайту — то есть что строка в page.js действительно работает как хук.

import assert from "node:assert/strict";
import { test } from "node:test";
import vm from "node:vm";

import { classicWorkerScript } from "../../src/page/worker-capture/script-composer.ts";
import { refuse, TRANSPARENT_POLICY, type CodecPolicy } from "../../src/shared/codec-policy.ts";
import { parseWorkerReport, WORKER_CHANNEL_HANDSHAKE } from "../../src/shared/worker-channel-contract.ts";
import { encodeWorkerHookConfig } from "../../src/shared/worker-hook-config.ts";
import { buildWorkerHookSource } from "../../scripts/build.ts";
import { createFakeWorkerScope } from "./support/fake-worker-scope.ts";

const AV1_MIME = 'video/mp4; codecs="av01.0.05M.08"';
const SITE_SCRIPT_URL = "https://site.example/js/player-worker.js";
const hookSource = await buildWorkerHookSource("chrome");
/** Байтов в этих донесениях нет: проверка байтов не нужна. */
const isNeverBytes = (_value: unknown): _value is Uint8Array => false;

/** Выполнить классический blob-скрипт обёртки; `siteScript` — «код сайта» под `importScripts`. */
function runClassicWorker(policy: CodecPolicy, siteScript: (scope: ReturnType<typeof createFakeWorkerScope>["scope"]) => void) {
  const fake = createFakeWorkerScope();
  const imported: string[] = [];
  const debugLines: string[] = [];
  // В воркере `self` — сам глобал: консоль хук берёт как `self.console`.
  Object.assign(fake.scope, { console: { debug: (line: string) => debugLines.push(line) } });
  const context = vm.createContext({
    self: fake.scope,
    // Есть в любом воркере; модули протокола в бандле снимают их на старте.
    TextEncoder,
    TextDecoder,
    console: { debug: (line: string) => debugLines.push(line) },
    importScripts: (url: string) => {
      imported.push(url);
      siteScript(fake.scope);
    },
  });
  const script = classicWorkerScript({ source: hookSource, configLiteral: encodeWorkerHookConfig({ codecPolicy: policy }) }, JSON.stringify(SITE_SCRIPT_URL));
  let thrown: unknown = null;
  try {
    vm.runInContext(script, context);
  } catch (error) {
    thrown = error;
  }
  return { fake, imported, debugLines, thrown };
}

test("the built hook bundle is an IIFE that names the config parameter and nothing else global", () => {
  assert.doesNotMatch(hookSource, /^\s*(import|export)\s/m);
  assert.match(hookSource, /\bfastiplayerWorkerHookConfig\b/);
});

test("the embedded policy answers codec questions before the site's first line runs", () => {
  let answerAtSiteStart: boolean | null = null;
  const { fake, imported, debugLines } = runClassicWorker(refuse(["av1"]), (scope) => {
    answerAtSiteStart = scope.MediaSource.isTypeSupported(AV1_MIME);
  });
  assert.deepEqual(imported, [SITE_SCRIPT_URL]);
  assert.equal(answerAtSiteStart, false, "the hook was installed before the site's script");
  assert.equal(fake.scope.MediaSource.isTypeSupported('video/mp4; codecs="avc1.42E01E"'), true);
  assert.deepEqual(debugLines, [], "the hook started without oddities");
  const transparent = runClassicWorker(TRANSPARENT_POLICY, () => {});
  assert.equal(transparent.fake.scope.MediaSource.isTypeSupported(AV1_MIME), true);
});

test("inside the real bundle the handshake is hidden from the site; the port gets hook_ready, then site_script_loaded", async () => {
  const siteSaw: unknown[] = [];
  const { fake } = runClassicWorker(TRANSPARENT_POLICY, (scope) => {
    scope.addEventListener("message", (event) => siteSaw.push((event as MessageEvent).data));
  });
  const channel = new MessageChannel();
  const reports: unknown[] = [];
  channel.port1.addEventListener("message", (event) => reports.push((event as MessageEvent).data));
  channel.port1.start();
  try {
    fake.deliverToWorker(WORKER_CHANNEL_HANDSHAKE, [channel.port2]);
    fake.deliverToWorker({ type: "init" });
    const deadline = Date.now() + 2000;
    while (reports.length === 0 && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 2));
    while (reports.length < 2 && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 2));
    assert.deepEqual(reports, [{ kind: "hook_ready" }, { kind: "site_script_loaded" }]);
    assert.ok(reports.every((report) => parseWorkerReport(report, isNeverBytes) !== undefined), "the strict contract accepts both reports");
    assert.deepEqual(siteSaw, [{ type: "init" }]);
  } finally {
    channel.port1.close();
  }
});

test("a throwing site script: its own error leaves the worker script unchanged, the loaded report still goes (finally)", async () => {
  const siteError = new Error("site script failed");
  const { fake, thrown } = runClassicWorker(TRANSPARENT_POLICY, () => {
    throw siteError;
  });
  assert.equal(thrown, siteError, "the site's exception is not replaced by ours");
  const channel = new MessageChannel();
  const reports: unknown[] = [];
  channel.port1.addEventListener("message", (event) => reports.push((event as MessageEvent).data));
  channel.port1.start();
  try {
    fake.deliverToWorker(WORKER_CHANNEL_HANDSHAKE, [channel.port2]);
    const deadline = Date.now() + 2000;
    while (reports.length < 2 && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 2));
    assert.deepEqual(reports, [{ kind: "hook_ready" }, { kind: "site_script_loaded" }]);
  } finally {
    channel.port1.close();
  }
});

test("the site never sees the links object: no new globals in the worker after our blob script", () => {
  let globalsAtSiteStart: string[] = [];
  const { fake } = runClassicWorker(TRANSPARENT_POLICY, (scope) => {
    globalsAtSiteStart = Object.getOwnPropertyNames(scope);
  });
  assert.ok(!globalsAtSiteStart.includes("fastiplayerWorkerHookLinks"));
  assert.ok(!globalsAtSiteStart.includes("fastiplayerWorkerHookConfig"));
  assert.ok(!Object.getOwnPropertyNames(fake.scope).includes("fastiplayerWorkerHookLinks"));
});
