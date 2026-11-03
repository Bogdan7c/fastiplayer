// Обёртка `Worker` в MAIN-мире (сессия 09b, пакет C): условие включения, текст blob-скриптов и
// порядок «хук → скрипт сайта», откат к оригиналу, прозрачность конструктора, рукопожатие первым
// сообщением, счёт сообщений и сопоставление handle → MediaSource воркера в обоих порядках прихода.
// Воркер — фейковый, но хук в нём НАСТОЯЩИЙ (tests/page/support/fake-worker-pair.ts).

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";

import type { PageBridge } from "../../src/page/bridge.ts";
import type { WorkerHookSource } from "../../src/page/worker-capture/index.ts";
import { refuse, TRANSPARENT_POLICY, type CodecPolicy } from "../../src/shared/codec-policy.ts";
import { WORKER_CHANNEL_MARKER_FIELD } from "../../src/shared/worker-channel-contract.ts";
import { startHarness } from "./support/bridge-harness.ts";
import { createFakePage, type FakePage } from "./support/fake-page.ts";
import { addWorkerSupport, closeWorkerChannels, FakeBlob, isWrapperScript, type RunningWorker, type WorkerPair, type WorkerPairOptions } from "./support/fake-worker-pair.ts";

const VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';

afterEach(closeWorkerChannels);
const HOOK_SOURCE = "/* worker hook bundle */";

interface WorkerWorld {
  readonly page: FakePage;
  readonly pair: WorkerPair;
  readonly bridge: PageBridge;
  readonly debugLines: string[];
  readonly policy: { value: CodecPolicy };
  /** Конструктор `Worker`, который видит страница (после установки обёртки). */
  readonly PageWorker: new (url: unknown, options?: unknown) => EventTarget & { postMessage(message: unknown, transfer?: unknown[]): void };
}

function startWorld(options: { pair?: WorkerPairOptions; withHook?: boolean } = {}): WorkerWorld {
  const page = createFakePage();
  const pair = addWorkerSupport(page, options.pair);
  const policy = { value: TRANSPARENT_POLICY as CodecPolicy };
  const workerHook: WorkerHookSource = { source: HOOK_SOURCE, policy: { current: () => policy.value } };
  const harness = startHarness({ page, ...(options.withHook === false ? {} : { workerHook }) });
  const PageWorker = (page.win as unknown as { Worker: WorkerWorld["PageWorker"] }).Worker;
  return { page, pair, bridge: harness.bridge, debugLines: harness.debugLines, policy, PageWorker };
}

async function until(condition: () => boolean, what: string, timeoutMs = 2000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!condition()) {
    if (Date.now() > deadline) throw new Error(`timeout waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 2));
  }
}

/** Наш blob-скрипт (в нём параметр настройки хука), а не blob страницы. */
function isOurBlob(page: FakePage, url: string): boolean {
  const subject = page.objectUrlSubjects.get(url);
  return subject instanceof FakeBlob && isWrapperScript(subject.text);
}

function blobText(page: FakePage, url: string): string {
  const subject = page.objectUrlSubjects.get(url);
  assert.ok(subject instanceof FakeBlob, `${url} is one of our blobs`);
  assert.equal(subject.type, "text/javascript");
  return subject.text;
}

/** Как плеер сайта в воркере: MediaSource, его handle — странице в сообщении. */
function postHandle(running: RunningWorker, wrap: (handle: object) => unknown = (handle) => ({ type: "handle", handle })) {
  const mediaSource = new running.fake.FakeMediaSource();
  const handle = mediaSource.handle;
  running.fake.scope.postMessage(wrap(handle), [handle]);
  return mediaSource;
}

/** Ловит сообщения воркера на стороне страницы (как слушатель сайта). */
function siteInbox(worker: EventTarget): unknown[] {
  const inbox: unknown[] = [];
  worker.addEventListener("message", (event) => inbox.push((event as MessageEvent).data));
  return inbox;
}

test("the wrapper is installed only when MediaSource can be constructed in a dedicated worker and a hook is built in", () => {
  const cases: { readonly pair: WorkerPairOptions; readonly withHook: boolean; readonly wrapped: boolean }[] = [
    { pair: { canConstructInDedicatedWorker: true }, withHook: true, wrapped: true },
    { pair: { canConstructInDedicatedWorker: false }, withHook: true, wrapped: false },
    { pair: { canConstructInDedicatedWorker: undefined }, withHook: true, wrapped: false }, // свойства нет (Firefox)
    { pair: { canConstructInDedicatedWorker: true }, withHook: false, wrapped: false },
  ];
  for (const testCase of cases) {
    const world = startWorld({ pair: testCase.pair, withHook: testCase.withHook });
    assert.equal(world.bridge.workersWrapped, testCase.wrapped, JSON.stringify(testCase));
    assert.equal(world.PageWorker === world.pair.FakeWorker, !testCase.wrapped, JSON.stringify(testCase));
  }
});

test("transparency: same prototype, instanceof, name, length, constructor, subclassing and the no-new error", () => {
  const { pair, PageWorker } = startWorld();
  const { FakeWorker } = pair;
  assert.notEqual(PageWorker, FakeWorker);
  assert.equal(PageWorker.prototype, FakeWorker.prototype);
  assert.equal(PageWorker.name, FakeWorker.name);
  assert.equal(PageWorker.length, FakeWorker.length);
  assert.equal(PageWorker.prototype.constructor, PageWorker);
  assert.equal(Object.getPrototypeOf(PageWorker), Object.getPrototypeOf(FakeWorker));
  const worker = new PageWorker(pair.siteBlobUrl("/js/player-worker.js"));
  assert.ok(worker instanceof FakeWorker && worker instanceof PageWorker);
  assert.match(pair.created.at(-1)?.url ?? "", /^blob:/, "the blob worker really went through the wrapper");
  class SiteWorker extends PageWorker {
    readonly label = "site";
  }
  const subclassed = new SiteWorker(pair.siteBlobUrl("/js/player-worker.js"));
  assert.ok(subclassed instanceof SiteWorker && subclassed instanceof FakeWorker);
  assert.equal(subclassed.label, "site");
  assert.throws(() => (PageWorker as unknown as (url: string) => unknown)(pair.siteBlobUrl("/js/player-worker.js")), TypeError);
});

test("classic worker from a page blob: our blob = hook invocation with the policy literal, then importScripts of the page's blob, then the loaded report", async () => {
  const world = startWorld();
  world.policy.value = refuse(["av1", "vp9"]);
  const siteUrl = world.pair.siteBlobUrl("js/player-worker.js?v=2");
  const worker = new world.PageWorker(siteUrl, { name: "player" });
  const running = world.pair.created[0];
  assert.ok(running !== undefined);
  assert.equal(running.worker, worker);
  assert.match(running.url, /^blob:/);
  assert.notEqual(running.url, siteUrl, "the browser got our blob, not the page's");
  assert.deepEqual(running.options, { name: "player" }, "the page's options reach the browser as they were");
  const text = blobText(world.page, running.url);
  assert.equal(
    text,
    "(function () {\n" +
      "var fastiplayerWorkerHookLinks = { siteScriptLoaded: function () {} };\n" +
      `(function (fastiplayerWorkerHookConfig, fastiplayerWorkerHookLinks) {\n${HOOK_SOURCE}\n})("av1,vp9", fastiplayerWorkerHookLinks);\n` +
      `try {\nimportScripts(${JSON.stringify(siteUrl)});\n} finally {\nfastiplayerWorkerHookLinks.siteScriptLoaded();\n}\n})();\n`,
  );
  await until(() => running.loadOrder.length === 2, "the worker to start");
  assert.deepEqual(running.loadOrder, ["hook", siteUrl]);
});

test("module worker from a page blob: the entry imports the hook module, then the page's module, then reports it loaded", async () => {
  const world = startWorld();
  const siteUrl = world.pair.siteBlobUrl("/js/module-worker.js", "module");
  new world.PageWorker(siteUrl, { type: "module", credentials: "include" });
  const running = world.pair.created[0];
  assert.ok(running !== undefined);
  assert.deepEqual(running.options, { type: "module", credentials: "include" });
  const entry = blobText(world.page, running.url);
  const entryShape = /^import \{ fastiplayerSiteScriptLoaded \} from "([^"]+)";\nimport "([^"]+)";\nfastiplayerSiteScriptLoaded\(\);\n$/.exec(entry);
  assert.ok(entryShape !== null, `entry module text: ${entry}`);
  const [, hookUrl, importedSite] = entryShape;
  assert.equal(importedSite, siteUrl);
  assert.ok(hookUrl !== undefined);
  assert.equal(
    blobText(world.page, hookUrl),
    "const fastiplayerWorkerHookLinks = { siteScriptLoaded: function () {} };\n" +
      `(function (fastiplayerWorkerHookConfig, fastiplayerWorkerHookLinks) {\n${HOOK_SOURCE}\n})("", fastiplayerWorkerHookLinks);\n` +
      "export function fastiplayerSiteScriptLoaded() {\nfastiplayerWorkerHookLinks.siteScriptLoaded();\n}\n",
  );
  await until(() => running.loadOrder.length === 2, "the worker to start");
  assert.deepEqual(running.loadOrder, ["hook", siteUrl]);
});

test("the policy is read at the moment each worker is constructed", () => {
  const world = startWorld();
  new world.PageWorker(world.pair.siteBlobUrl("/a.js"));
  world.policy.value = refuse(["av1"]);
  new world.PageWorker(world.pair.siteBlobUrl("/b.js"));
  const literals = world.pair.created.map((running) => /\}\)\(("[^"]*"), fastiplayerWorkerHookLinks\);/.exec(blobText(world.page, running.url))?.[1]);
  assert.deepEqual(literals, ['""', '"av1"']);
});

test("not wrapped (decision 192): same-origin http(s), another origin, data:, a foreign-origin blob and Trusted Types URLs go to the original as they were", () => {
  class TrustedScriptURL {
    toString(): string {
      return "/trusted-worker.js";
    }
  }
  // TrustedScriptURL снимается на старте: мир собирается с ним.
  const page = createFakePage();
  const pair = addWorkerSupport(page);
  Object.assign(page.win, { TrustedScriptURL });
  startHarness({ page, workerHook: { source: HOOK_SOURCE, policy: { current: () => TRANSPARENT_POLICY } } });
  const PageWorker = (page.win as unknown as { Worker: WorkerWorld["PageWorker"] }).Worker;
  const trusted = new TrustedScriptURL();
  const urls = [
    "/js/player-worker.js",
    "https://site.example/js/module-worker.js",
    "https://cdn.example/worker.js",
    "data:text/javascript,1",
    "blob:https://cdn.example/4f1c",
    trusted,
  ];
  for (const url of urls) new PageWorker(url);
  assert.deepEqual(
    pair.created.map((running) => running.url),
    ["/js/player-worker.js", "https://site.example/js/module-worker.js", "https://cdn.example/worker.js", "data:text/javascript,1", "blob:https://cdn.example/4f1c", "/trusted-worker.js"],
  );
  assert.equal(page.objectUrlSubjects.size, 0, "no blob of ours was even created");
});

test("a blob worker refused by the browser (CSP) falls back to the original worker; the page's worker never breaks", async () => {
  const world = startWorld();
  const cspError = new Error("SecurityError: worker-src");
  // CSP отвергает именно НАШ blob (в нём хук); blob страницы и без нас проходил бы.
  world.pair.constructorFailure.reject = (url) => (isOurBlob(world.page, url) ? cspError : null);
  const siteUrl = world.pair.siteBlobUrl("/js/player-worker.js");
  const worker = new world.PageWorker(siteUrl);
  assert.equal(world.pair.created.length, 1, "exactly one worker was created");
  const running = world.pair.created[0];
  assert.ok(running !== undefined);
  assert.equal(running.worker, worker);
  assert.equal(running.url, siteUrl, "the original arguments, untouched");
  assert.deepEqual(world.page.revokedObjectUrls.length, 1, "our blob URL is released");
  assert.ok(world.debugLines.some((line) => line.includes("worker could not be wrapped")));
  await until(() => running.loadOrder.length === 1, "the original worker to start");
  assert.deepEqual(running.loadOrder, [siteUrl], "no hook inside: the media source is honestly untracked");
});

test("when the original constructor throws too, the page gets the original error", () => {
  const world = startWorld();
  const original = new Error("SyntaxError: bad url");
  world.pair.constructorFailure.reject = () => original;
  assert.throws(() => new world.PageWorker(world.pair.siteBlobUrl("/js/player-worker.js")), (error: unknown) => error === original);
});

test("the channel handshake is the worker's first message and the site in the worker never sees it", async () => {
  const world = startWorld();
  const siteSaw: unknown[] = [];
  world.pair.behaviour.onSiteScript = (running) => running.fake.scope.addEventListener("message", (event) => siteSaw.push((event as MessageEvent).data));
  const worker = new world.PageWorker(world.pair.siteBlobUrl("/js/player-worker.js"));
  worker.postMessage({ type: "init" });
  const running = world.pair.created[0];
  assert.ok(running !== undefined);
  await until(() => siteSaw.length === 1, "the site's message");
  assert.deepEqual(siteSaw, [{ type: "init" }]);
  // Подделка метки страницей позже — обычное сообщение сайта.
  worker.postMessage({ [WORKER_CHANNEL_MARKER_FIELD]: 1 });
  await until(() => siteSaw.length === 2, "the forged marker");
  assert.deepEqual(running.oddities, []);
  // Хук принял порт: blob-адреса отпущены по `hook_ready`.
  await until(() => world.page.revokedObjectUrls.includes(running.url), "blob URL release on hook_ready");
});

test("handle → worker media source: lazy, whichever arrives first — the site's message or the hook's report", async () => {
  for (const order of ["site message first", "hook report first"] as const) {
    const world = startWorld();
    world.pair.behaviour.pageDeliveryDelayMs = order === "hook report first" ? 40 : 0;
    const worker = new world.PageWorker(world.pair.siteBlobUrl("/js/player-worker.js"));
    const inbox = siteInbox(worker);
    const running = world.pair.created[0];
    assert.ok(running !== undefined);
    await until(() => running.loadOrder.length === 2, "the worker to start");
    running.fake.scope.postMessage({ type: "stats" }); // сообщение 1, без handle
    const mediaSource = postHandle(running); // сообщение 2
    if (order === "site message first") await until(() => inbox.length === 2, "the site's messages");
    mediaSource.testOpen();
    mediaSource.addSourceBuffer(VIDEO_MIME);
    await until(() => inbox.length === 2, "the site's messages");
    const handle = (inbox[1] as { handle: object }).handle;
    await until(() => world.bridge.workerMediaSources.resolveHandle(handle)?.kind === "live", `live resolution (${order})`);
    const resolution = world.bridge.workerMediaSources.resolveHandle(handle);
    assert.equal(resolution?.kind === "live" ? resolution.mediaSource.key : null, "w1/ms-1", order);
    assert.deepEqual(inbox[0], { type: "stats" }, "the site gets its messages untouched");
  }
});

test("worker numbers namespace the keys; a media source without a source buffer is not live yet", async () => {
  const world = startWorld();
  const first = new world.PageWorker(world.pair.siteBlobUrl("/a.js"));
  const second = new world.PageWorker(world.pair.siteBlobUrl("/b.js"));
  const inboxes = [siteInbox(first), siteInbox(second)];
  await until(() => world.pair.created.every((running) => running.loadOrder.length === 2), "both workers to start");
  const [runningA, runningB] = world.pair.created;
  assert.ok(runningA !== undefined && runningB !== undefined);
  const waiting = postHandle(runningB);
  await until(() => inboxes[1]?.length === 1, "worker b's handle");
  const handleB = (inboxes[1]?.[0] as { handle: object }).handle;
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.deepEqual(world.bridge.workerMediaSources.resolveHandle(handleB), { kind: "not_live" });
  waiting.testOpen();
  waiting.addSourceBuffer(VIDEO_MIME);
  await until(() => world.bridge.workerMediaSources.resolveHandle(handleB)?.kind === "live", "worker b live");
  const resolution = world.bridge.workerMediaSources.resolveHandle(handleB);
  assert.equal(resolution?.kind === "live" ? resolution.mediaSource.key : null, "w2/ms-1");
  // Закрыт — снова «пока нечего».
  waiting.testClose();
  await until(() => world.bridge.workerMediaSources.resolveHandle(handleB)?.kind === "not_live", "closed media source");
});

test("untracked handles: never seen, too deep, beyond the scan budget, two in one message — honest unsupported", async () => {
  const world = startWorld();
  const worker = new world.PageWorker(world.pair.siteBlobUrl("/js/player-worker.js"));
  const inbox = siteInbox(worker);
  const running = world.pair.created[0];
  assert.ok(running !== undefined);
  await until(() => running.loadOrder.length === 2, "the worker to start");
  const shapes: ((handle: object) => unknown)[] = [
    (handle) => ({ a: { b: { c: { d: { e: handle } } } } }), // глубина 5
    (handle) => ({ list: [...Array.from({ length: 300 }, (_, index) => index), handle] }), // за пределом обхода
  ];
  const mediaSources = shapes.map((shape) => postHandle(running, shape));
  const twin = [new running.fake.FakeMediaSource(), new running.fake.FakeMediaSource()];
  running.fake.scope.postMessage({ pair: [twin[0]?.handle, twin[1]?.handle] }, [twin[0]?.handle, twin[1]?.handle]);
  for (const mediaSource of [...mediaSources, ...twin]) {
    mediaSource.testOpen();
    mediaSource.addSourceBuffer(VIDEO_MIME);
  }
  await until(() => inbox.length === 3, "the site's messages");
  await new Promise((resolve) => setTimeout(resolve, 20));
  const deep = (inbox[0] as { a: { b: { c: { d: { e: object } } } } }).a.b.c.d.e;
  const late = (inbox[1] as { list: object[] }).list[300];
  const pairHandles = (inbox[2] as { pair: object[] }).pair;
  const registry = world.bridge.workerMediaSources;
  for (const handle of [deep, late, ...pairHandles]) assert.deepEqual(registry.resolveHandle(handle), { kind: "untracked" });
  assert.deepEqual(registry.resolveHandle(new world.pair.FakeMediaSourceHandle()), { kind: "untracked" }, "a handle we never saw");
  assert.equal(registry.resolveHandle({}), null, "not a handle at all");
});

test("synthetic message events from the page do not shift the message count", async () => {
  const world = startWorld();
  const worker = new world.PageWorker(world.pair.siteBlobUrl("/js/player-worker.js"));
  const inbox = siteInbox(worker);
  const running = world.pair.created[0];
  assert.ok(running !== undefined);
  await until(() => running.loadOrder.length === 2, "the worker to start");
  // Страница сама диспатчит «сообщение» на объект Worker (недоверенное).
  worker.dispatchEvent(new MessageEvent("message", { data: { fake: true } }));
  const mediaSource = postHandle(running);
  mediaSource.testOpen();
  mediaSource.addSourceBuffer(VIDEO_MIME);
  await until(() => inbox.length === 2, "the site's messages");
  const handle = (inbox[1] as { handle: object }).handle;
  await until(() => world.bridge.workerMediaSources.resolveHandle(handle)?.kind === "live", "live resolution");
});

// ---------------------------------------------------------------------------------------------
// Отложенный revoke blob-адреса сайта (решение 192, находка 1 ревью 09b).
// ---------------------------------------------------------------------------------------------

/** `URL.revokeObjectURL`, каким его видит страница (после установки обёртки). */
function pageRevoke(page: FakePage): (url: unknown) => unknown {
  const pageUrl = page.win.URL as unknown as { revokeObjectURL(url: unknown): unknown };
  return (url) => pageUrl.revokeObjectURL(url);
}

for (const kind of ["classic", "module"] as const) {
  test(`${kind}: revoke right after construction (Vite, worker-loader) — the site's worker still loads, the real revoke follows its loaded report`, async () => {
    const world = startWorld();
    const siteUrl = world.pair.siteBlobUrl("/js/player-worker.js", kind);
    let worker: EventTarget | undefined;
    try {
      worker = new world.PageWorker(siteUrl, kind === "module" ? { type: "module" } : undefined);
    } finally {
      assert.equal(pageRevoke(world.page)(siteUrl), undefined, "revoke answers like the original");
    }
    assert.ok(worker !== undefined);
    assert.equal(world.page.revokedObjectUrls.includes(siteUrl), false, "the real revoke waits for our worker");
    const running = world.pair.created[0];
    assert.ok(running !== undefined);
    await until(() => running.loadOrder.length === 2, "the worker to start");
    assert.deepEqual(running.loadOrder, ["hook", siteUrl], "the site's script loaded from its (already revoked by the page) blob");
    await until(() => world.page.revokedObjectUrls.includes(siteUrl), "the deferred revoke");
    assert.equal(world.pair.pendingSiteScriptDeadlines(), 0, "the deadline is cancelled by the report");
  });
}

test("no loaded report (the hook did not start) — the deferred revoke happens at the deadline", async () => {
  const world = startWorld();
  world.pair.behaviour.reportSiteScriptLoaded = false;
  const siteUrl = world.pair.siteBlobUrl("/js/player-worker.js");
  new world.PageWorker(siteUrl);
  pageRevoke(world.page)(siteUrl);
  const running = world.pair.created[0];
  assert.ok(running !== undefined);
  await until(() => running.loadOrder.length === 2, "the worker to start");
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(world.page.revokedObjectUrls.includes(siteUrl), false, "still held: no report yet");
  assert.equal(world.pair.pendingSiteScriptDeadlines(), 1);
  world.pair.fireSiteScriptDeadlines();
  assert.ok(world.page.revokedObjectUrls.includes(siteUrl), "revoked at the deadline");
});

test("URLs our workers do not hold revoke immediately, as without us (a hold elsewhere does not delay them)", async () => {
  const world = startWorld();
  world.pair.behaviour.reportSiteScriptLoaded = false; // удержание стоит весь тест
  const heldUrl = world.pair.siteBlobUrl("/js/player-worker.js");
  new world.PageWorker(heldUrl);
  const otherUrl = world.pair.siteBlobUrl("/js/other.js");
  const revoke = pageRevoke(world.page);
  assert.equal(revoke(otherUrl), undefined);
  assert.equal(world.page.revokedObjectUrls.at(-1), otherUrl, "another blob URL is revoked at once");
  revoke("https://site.example/not-a-blob");
  assert.equal(world.page.revokedObjectUrls.at(-1), "https://site.example/not-a-blob", "a non-blob string goes to the original");
  let conversions = 0;
  const objectArgument = {
    toString(): string {
      conversions += 1;
      return heldUrl;
    },
  };
  revoke(objectArgument);
  assert.equal(conversions, 1, "a non-string argument is converted once — by the original, not by us");
  const pageUrl = world.page.win.URL as unknown as { revokeObjectURL: Function };
  assert.equal(pageUrl.revokeObjectURL.name, "revokeObjectURL");
  assert.equal(pageUrl.revokeObjectURL.length, 1);
  assert.throws(() => new (pageUrl.revokeObjectURL as unknown as new () => unknown)(), TypeError, "not a constructor, like the original");
});

test("a URL the page revoked BEFORE new Worker stays dead: the worker fails to load, as without us", async () => {
  const world = startWorld();
  const siteUrl = world.pair.siteBlobUrl("/js/player-worker.js");
  pageRevoke(world.page)(siteUrl);
  new world.PageWorker(siteUrl);
  const running = world.pair.created[0];
  assert.ok(running !== undefined);
  await until(() => running.loadOrder.length === 2, "the worker to start");
  assert.deepEqual(running.loadOrder, ["hook", `failed:${siteUrl}`]);
});

test("without a deferrable revokeObjectURL the Worker wrapper is not installed (the site's workers stay untouched)", () => {
  const page = createFakePage();
  const pair = addWorkerSupport(page);
  Object.defineProperty(page.win.URL, "revokeObjectURL", { value: () => undefined, writable: false, configurable: false });
  const harness = startHarness({ page, workerHook: { source: HOOK_SOURCE, policy: { current: () => TRANSPARENT_POLICY } } });
  assert.equal(harness.bridge.workersWrapped, false);
  assert.equal((page.win as unknown as { Worker: unknown }).Worker, pair.FakeWorker);
  assert.ok(harness.debugLines.some((line) => line.includes("blob revoke could not be deferred")));
});
