// Загрузчики источников сеанса (session-source-feeds.ts) на настоящих правилах Referer и заслона:
// отсутствующий ресурс, активный HLS и файл, отказ адреса, учёт номерков между поколениями,
// обвязка сети и безопасность Referer при параллельных запросах.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { FileAddressRule } from "../../src/background/handoff/file-address-guard.ts";
import { createFileReferrerRules, exactUrlFilter } from "../../src/background/handoff/file-referrer-rule.ts";
import { createLocalAddressBlock, localAddressBlockRules } from "../../src/background/handoff/local-address-block-rules.ts";
import type { OutgoingItem } from "../../src/background/handoff/page-event-translator.ts";
import type { RangeFeedSink, RangeFetch, RangeFetchInit } from "../../src/background/handoff/range-fetcher.ts";
import { createSessionRules, FIRST_REQUEST_RULE_ID, SESSION_RULE_PRIORITY } from "../../src/background/handoff/session-rules.ts";
import { createSessionSourceFeeds, type PulledSourceStart } from "../../src/background/handoff/session-source-feeds.ts";
import type { ResourceFetchRequest } from "../../src/background/handoff/hls/resource-fetcher.ts";
import { formatResourceAddress } from "../../src/protocol/index.ts";
import { createFakeSessionRules, EXTENSION_HOST } from "./handoff-world.ts";
import { createFakeClock, flushPromises } from "./support.ts";

const PUBLIC_RULE: FileAddressRule = { pageHost: "video.example.com", setting: "block", consentedHost: null };
const LOCAL_PAGE_RULE: FileAddressRule = { pageHost: "nas.local", setting: "block", consentedHost: null };
const ROOT_URL = "https://cdn.example.net/vod/master.m3u8";
const FILE_URL = "https://cdn.example.net/movie.mp4";
const PLAYLIST = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nseg0.ts\n#EXT-X-ENDLIST\n";

async function settle(): Promise<void> {
  for (let round = 0; round < 30; round += 1) await flushPromises();
}

/** Сеть: плейлист по корню, остальное - 8 байт; записывает обращения и снимок правил в миг запроса. */
function recordingNetwork(snapshotRules: () => readonly { id: number; filter: string | undefined; priority: number }[] = () => []) {
  const calls: { url: string; init: RangeFetchInit; rulesAtCall: readonly { id: number; filter: string | undefined; priority: number }[] }[] = [];
  const fetch: RangeFetch = (url, init) => {
    calls.push({ url, init, rulesAtCall: snapshotRules() });
    const text = url === ROOT_URL ? PLAYLIST : "12345678";
    const response = new Response(text, { status: 200 });
    Object.defineProperty(response, "url", { value: url });
    return Promise.resolve(response);
  };
  return { fetch, calls };
}

function build(fetch: RangeFetch) {
  const clock = createFakeClock();
  const rules = createFakeSessionRules();
  const requestRules = createSessionRules({
    api: rules.api,
    scheduler: clock.scheduler,
    permanentRules: localAddressBlockRules(EXTENSION_HOST),
    reportFailureOnce: () => {},
  });
  const feeds = createSessionSourceFeeds({
    fetch,
    fileReferrer: createFileReferrerRules(requestRules),
    localAddressBlock: createLocalAddressBlock(requestRules, EXTENSION_HOST),
    scheduler: clock.scheduler,
  });
  const items: OutgoingItem[] = [];
  const sink: RangeFeedSink = { enqueue: (item) => void items.push(item), queuedBytes: () => 0 };
  const start = (url: string, rule: FileAddressRule = PUBLIC_RULE, referrer: PulledSourceStart["referrer"] = null): PulledSourceStart => ({ url, referrer, rule, sink });
  return { feeds, rules, items, sink, start };
}

function whole(requestId: number, resource: number): ResourceFetchRequest {
  return { request_id: requestId, resource, byte_range: { kind: "whole" }, max_bytes: 1_000_000 };
}

const finishedOutcomes = (items: readonly OutgoingItem[]) =>
  items.flatMap((item) => (item.kind === "message" && item.message.type === "resource_fetch_finished" ? [{ id: item.message.request_id, outcome: item.message.outcome }] : []));

describe("источники сеанса: нет ресурса", () => {
  it("fetch_resource без HLS: refused сразу, сеть не тронута; fetch_range без файла - ничего", async () => {
    const network = recordingNetwork();
    const { feeds, items, sink } = build(network.fetch);
    feeds.requestResource(whole(3, 0), sink);
    feeds.requestRange({ request_id: 4, offset: 0, length: 10 });
    await settle();
    assert.deepEqual(finishedOutcomes(items), [{ id: 3, outcome: { kind: "failed", failure: { kind: "refused" } } }]);
    assert.equal(items.length, 1, "на запрос байт файла ничего не ушло");
    assert.equal(network.calls.length, 0);
  });
});

describe("источники сеанса: активный HLS", () => {
  it("startHls выдаёт корень 0, запрос корня отдаёт переписанный плейлист, а его сегмент - байты", async () => {
    const network = recordingNetwork();
    const { feeds, items, sink, start } = build(network.fetch);
    assert.deepEqual(feeds.startHls(start(ROOT_URL)), { kind: "started", root: 0 });
    feeds.requestResource(whole(1, 0), sink);
    await settle();
    const frame = items.find((item) => item.kind === "frame");
    assert.ok(frame?.kind === "frame");
    assert.equal(new TextDecoder().decode(frame.payload), PLAYLIST.replace("seg0.ts", formatResourceAddress(1)));
    feeds.requestResource(whole(2, 1), sink);
    await settle();
    assert.deepEqual(finishedOutcomes(items).map((entry) => entry.outcome.kind), ["completed", "completed"]);
    assert.equal(network.calls[1]?.url, "https://cdn.example.net/vod/seg0.ts");
  });

  it("корень на запрещённом адресе: address_refused, сети нет, а прежний файл жив и качает дальше (чужое состояние не тронуто)", async () => {
    const network = recordingNetwork();
    const { feeds, items, start } = build(network.fetch);
    feeds.startFile(start(FILE_URL));
    assert.deepEqual(feeds.startHls(start("http://192.168.1.1/m.m3u8")), { kind: "address_refused" });
    assert.deepEqual(feeds.startHls(start("ftp://cdn.example.net/m.m3u8")), { kind: "address_refused" });
    feeds.requestRange({ request_id: 1, offset: 0, length: 4 });
    await settle();
    assert.deepEqual(network.calls.map((call) => call.url), [FILE_URL], "качался только файл");
    assert.ok(items.some((item) => item.kind === "message" && item.message.type === "range_fetch_finished"), "файл ответил на запрос");
  });

  it("endGeneration: загрузка оборвана, номерок прошлого поколения refused; следующий корень растёт", async () => {
    const hung: RangeFetchInit[] = [];
    const fetch: RangeFetch = (_url, init) => {
      hung.push(init);
      return new Promise((_resolve, reject) => init.signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError"))));
    };
    const { feeds, items, sink, start } = build(fetch);
    assert.deepEqual(feeds.startHls(start(ROOT_URL)), { kind: "started", root: 0 });
    feeds.requestResource(whole(1, 0), sink);
    await settle();
    assert.equal(hung[0]?.signal.aborted, false);
    feeds.endGeneration();
    await settle();
    assert.equal(hung[0]?.signal.aborted, true);
    assert.deepEqual(finishedOutcomes(items), [], "оборванный запрос итога не даёт");
    feeds.requestResource(whole(2, 0), sink);
    assert.deepEqual(finishedOutcomes(items), [{ id: 2, outcome: { kind: "failed", failure: { kind: "refused" } } }]);
    assert.deepEqual(feeds.startHls(start(ROOT_URL)), { kind: "started", root: 1 });
  });

  it("dispose обрывает идущую загрузку HLS; повторный dispose безопасен", async () => {
    const aborted: boolean[] = [];
    const fetch: RangeFetch = (_url, init) => {
      init.signal.addEventListener("abort", () => aborted.push(true));
      return new Promise(() => {});
    };
    const { feeds, sink, start } = build(fetch);
    feeds.startHls(start(ROOT_URL));
    feeds.requestResource(whole(1, 0), sink);
    await settle();
    feeds.dispose();
    feeds.dispose();
    assert.deepEqual(aborted, [true]);
  });
});

describe("источники сеанса: обвязка сети и Referer", () => {
  const REFERRER = { origin: "https://site.example", policy: "strict-origin-when-cross-origin" } as const;

  it("параллельные запросы (три на один сегмент): у каждого своё правило Referer с точным адресом и своим номером, все сняты после ответов", async () => {
    const holder: { snapshot: () => readonly { id: number; filter: string | undefined; priority: number }[] } = { snapshot: () => [] };
    const network = recordingNetwork(() => holder.snapshot());
    const world = build(network.fetch);
    holder.snapshot = () =>
      [...world.rules.rules.values()].flatMap((rule) => (rule !== undefined && rule.id >= FIRST_REQUEST_RULE_ID ? [{ id: rule.id, filter: rule.condition.urlFilter, priority: rule.priority }] : []));
    world.feeds.startHls(world.start(ROOT_URL, PUBLIC_RULE, REFERRER));
    world.feeds.requestResource(whole(1, 0), world.sink);
    await settle();
    // Дочерние адреса выданы переписанным плейлистом: берём 1 (сегмент); просим его несколько раз сразу.
    for (const requestId of [2, 3, 4]) world.feeds.requestResource({ ...whole(requestId, 1) }, world.sink);
    await settle();
    const segmentCalls = network.calls.filter((call) => call.url.endsWith("seg0.ts"));
    assert.equal(segmentCalls.length, 3);
    for (const call of segmentCalls) {
      const mine = call.rulesAtCall.filter((rule) => rule.filter === exactUrlFilter(call.url));
      assert.ok(mine.length >= 1, "правило с точным адресом стоит в момент запроса");
      assert.ok(mine.every((rule) => rule.priority === SESSION_RULE_PRIORITY.header));
    }
    const ids = world.rules.addedRequestRules().map((rule) => rule.id);
    assert.equal(new Set(ids).size, ids.length, "номера правил не повторяются");
    assert.equal(world.rules.requestRuleCount(), 0, "правила на запрос сняты");
    const rootCall = network.calls[0];
    assert.ok(rootCall !== undefined);
    assert.deepEqual(
      world.rules.addedRequestRules().filter((rule) => rule.condition.urlFilter === exactUrlFilter(ROOT_URL)).map((rule) => rule.action),
      [{ type: "modifyHeaders", requestHeaders: [{ header: "Referer", operation: "set", value: "https://site.example/" }] }],
    );
  });

  it("локальная страница: у запроса стоит и разрешение заслона, и Referer; публичная - только Referer", async () => {
    for (const [rule, expectsAllow] of [[LOCAL_PAGE_RULE, true], [PUBLIC_RULE, false]] as const) {
      const holder: { snapshot: () => readonly { id: number; filter: string | undefined; priority: number }[] } = { snapshot: () => [] };
      const network = recordingNetwork(() => holder.snapshot());
      const world = build(network.fetch);
      holder.snapshot = () =>
        [...world.rules.rules.values()].flatMap((added) => (added !== undefined && added.id >= FIRST_REQUEST_RULE_ID ? [{ id: added.id, filter: added.condition.urlFilter, priority: added.priority }] : []));
      world.feeds.startFile(world.start(FILE_URL, rule, REFERRER));
      world.feeds.requestRange({ request_id: 1, offset: 0, length: 4 });
      await settle();
      const priorities = network.calls[0]?.rulesAtCall.map((entry) => entry.priority).sort();
      assert.deepEqual(priorities, expectsAllow ? [SESSION_RULE_PRIORITY.allow, SESSION_RULE_PRIORITY.header] : [SESSION_RULE_PRIORITY.header]);
    }
  });
});
