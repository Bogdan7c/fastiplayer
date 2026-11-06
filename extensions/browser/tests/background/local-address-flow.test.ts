// Адрес файла через весь фон (решение 199): отказ до сеанса, вопрос человеку, повторная проверка
// адреса от страницы и перенаправления; Referer закачки (решение 202).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { LocalAddressConsent } from "../../src/background/handoff/file-address-guard.ts";
import { createHandOffCommandListener, parseHandOffTabRequest } from "../../src/background/handoff/hand-off-command.ts";
import type { RangeFetch } from "../../src/background/handoff/range-fetcher.ts";
import { FIRST_REQUEST_RULE_ID } from "../../src/background/handoff/session-rules.ts";
import { SESSION_CLOSE_CODES } from "../../src/protocol/index.ts";
import type { HandOffTabOutcome, VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { PLAYING_VIDEO, setup, startHandoff, TAB, type WorldOptions } from "./handoff-world.ts";
import { flushPromises } from "./support.ts";

const LOCAL_URL = "http://192.168.1.5/movie.mp4";
const PUBLIC_URL = "https://cdn.example/movie.mp4";
const LOCAL_FILE_VIDEO: VideoCandidate = { ...PLAYING_VIDEO, media: { kind: "file" }, frame_host: "video.example.com", file_host: "192.168.1.5" };
const PUBLIC_FILE_VIDEO: VideoCandidate = { ...LOCAL_FILE_VIDEO, file_host: "cdn.example" };
const CONSENT_FOR_FILE: LocalAddressConsent = { kind: "given_for", host: "192.168.1.5" };
const END_AS_SHADOW = { kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } };

function worldWith(candidate: VideoCandidate, options: WorldOptions = {}, fetchRange?: RangeFetch) {
  return setup([{ frameId: 0, result: [candidate] }], fetchRange, undefined, options);
}

async function settle(): Promise<void> {
  for (let round = 0; round < 20; round += 1) await flushPromises();
}

/** Ответ 206 на весь запрошенный диапазон, с конечным адресом `finalUrl`. */
function fileResponse(finalUrl: string, init: { readonly headers: Readonly<Record<string, string>> }) {
  const range = /bytes=(\d+)-(\d+)/.exec(init.headers["Range"] ?? "");
  const first = Number(range?.[1] ?? 0);
  const last = Number(range?.[2] ?? 0);
  const body = new Uint8Array(last - first + 1);
  const response = new Response(body, { status: 206, headers: { "content-range": `bytes ${first}-${last}/1000` } });
  Object.defineProperty(response, "url", { value: finalUrl });
  return response;
}

describe("проверка адреса при выборе видео", () => {
  it("«блокировать» (по умолчанию): отказ ДО сеанса — плеер не будится, страница не тронута, причина в модели", async () => {
    const world = worldWith(LOCAL_FILE_VIDEO);
    assert.deepEqual(await world.handoffs.handOffTab(TAB), { kind: "local_address_refused" });
    assert.equal(world.door.requests.length, 0);
    assert.equal(world.door.demandAcquired(), 0);
    assert.equal(world.framePorts.ports.length, 0);
    assert.deepEqual(world.handoffs.activeTabIds(), []);
    assert.deepEqual(await world.tabModel.lastRefusalOf(TAB), { kind: "local_address_refused" });
  });

  it("«спрашивать» без согласия: вопрос про этот хост, плеер не будится", async () => {
    const world = worldWith(LOCAL_FILE_VIDEO, { localAddressSetting: "ask" });
    const outcome = { kind: "local_address_needs_consent", host: "192.168.1.5" };
    assert.deepEqual(await world.handoffs.handOffTab(TAB), outcome);
    assert.equal(world.door.requests.length, 0);
    assert.deepEqual(await world.tabModel.lastRefusalOf(TAB), outcome);
  });

  it("«спрашивать» с согласием: сеанс открывается", async () => {
    const world = worldWith(LOCAL_FILE_VIDEO, { localAddressSetting: "ask" });
    assert.deepEqual(await startHandoff(world, CONSENT_FOR_FILE), { kind: "started", player_window: "new" });
  });

  it("человек согласился на один хост, а страница сменила файл: сеанса нет, новый вопрос про новый хост", async () => {
    const world = worldWith({ ...LOCAL_FILE_VIDEO, file_host: "192.168.1.1" }, { localAddressSetting: "ask" });
    const outcome = { kind: "local_address_needs_consent", host: "192.168.1.1" };
    assert.deepEqual(await world.handoffs.handOffTab(TAB, CONSENT_FOR_FILE), outcome);
    assert.equal(world.door.requests.length, 0);
    assert.deepEqual(await world.tabModel.lastRefusalOf(TAB), outcome);
  });

  it("согласие на чужой хост не даёт сеансу локального доступа: страница с публичным файлом не назовёт согласованный адрес", async () => {
    const world = worldWith(PUBLIC_FILE_VIDEO, { localAddressSetting: "ask" });
    await startHandoff(world, CONSENT_FOR_FILE);
    world.framePorts.last().deliver({ kind: "file_source", url: LOCAL_URL, referrer_origin: null, referrer_policy: "no-referrer" });
    assert.deepEqual(world.ended, [{ kind: "local_address_refused" }]);
  });

  it("согласие не отменяет «блокировать»", async () => {
    const world = worldWith(LOCAL_FILE_VIDEO);
    assert.deepEqual(await world.handoffs.handOffTab(TAB, CONSENT_FOR_FILE), { kind: "local_address_refused" });
  });

  it("локальная страница (Jellyfin, NAS) работает как обычно", async () => {
    const world = worldWith({ ...LOCAL_FILE_VIDEO, frame_host: "nas.local" });
    assert.deepEqual(await startHandoff(world), { kind: "started", player_window: "new" });
  });

  it("публичный файл и MSE проверку проходят при «блокировать»", async () => {
    assert.deepEqual(await startHandoff(worldWith(PUBLIC_FILE_VIDEO)), { kind: "started", player_window: "new" });
    assert.deepEqual(await startHandoff(setup()), { kind: "started", player_window: "new" });
  });

  it("настройка не читается: действует «блокировать», сбой виден, передача не падает", async () => {
    const world = worldWith(LOCAL_FILE_VIDEO, { localAddressSetting: Promise.reject(new Error("storage is gone")) });
    assert.deepEqual(await world.handoffs.handOffTab(TAB), { kind: "local_address_refused" });
    assert.equal(world.settingFailures.length, 1);
  });
});

describe("проверка адреса в сеансе", () => {
  it("кандидат сказал «публичный», а file_source назвал локальный: сеанс 4003, ни одного запроса в сеть", async () => {
    const calls: string[] = [];
    const world = worldWith(PUBLIC_FILE_VIDEO, {}, (url, init) => {
      calls.push(url);
      return Promise.resolve(fileResponse(url, init));
    });
    await startHandoff(world);
    const port = world.framePorts.last();
    port.deliver({ kind: "file_source", url: LOCAL_URL, referrer_origin: null, referrer_policy: "no-referrer" });
    world.sockets.last().receive({ type: "fetch_range", request_id: 1, offset: 0, length: 4 });
    await settle();
    assert.deepEqual(calls, []);
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.protocol_violation);
    assert.deepEqual(port.posted.at(-1), END_AS_SHADOW);
    assert.deepEqual(world.ended, [{ kind: "local_address_refused" }]);
    assert.deepEqual(await world.tabModel.lastRefusalOf(TAB), { kind: "local_address_refused" });
    assert.ok(!world.sockets.last().sent.some((payload) => typeof payload === "string" && payload.includes("file_source_added")), "поколение-файл плееру не объявлено");
  });

  it("согласие на один локальный хост не открывает другой", async () => {
    const world = worldWith(LOCAL_FILE_VIDEO, { localAddressSetting: "ask" });
    await startHandoff(world, CONSENT_FOR_FILE);
    world.framePorts.last().deliver({ kind: "file_source", url: "http://192.168.1.1/admin", referrer_origin: null, referrer_policy: "no-referrer" });
    assert.deepEqual(world.ended, [{ kind: "local_address_refused" }]);
  });

  it("согласованный хост проходит и в сеансе", async () => {
    const world = worldWith(LOCAL_FILE_VIDEO, { localAddressSetting: "ask" });
    await startHandoff(world, CONSENT_FOR_FILE);
    world.framePorts.last().deliver({ kind: "file_source", url: LOCAL_URL, referrer_origin: null, referrer_policy: "no-referrer" });
    assert.deepEqual(world.ended, []);
  });

  it("зависший API правил не держит загрузчик: перемотка плеера вытесняет запрос, и новый запрос идёт", async () => {
    let requestRuleInstalls = 0;
    const fetched: string[] = [];
    const world = worldWith(LOCAL_FILE_VIDEO, { localAddressSetting: "ask" }, (url, init) => {
      fetched.push(url);
      return Promise.resolve(fileResponse(url, init));
    });
    const realUpdate = world.sessionRules.api.updateSessionRules;
    world.sessionRules.api.updateSessionRules = (options) => {
      if (!(options.addRules ?? []).some((rule) => rule.id >= FIRST_REQUEST_RULE_ID)) return realUpdate(options);
      requestRuleInstalls += 1;
      return requestRuleInstalls === 1 ? new Promise(() => {}) : realUpdate(options);
    };
    await startHandoff(world, CONSENT_FOR_FILE);
    world.framePorts.last().deliver({ kind: "file_source", url: LOCAL_URL, referrer_origin: null, referrer_policy: "no-referrer" });
    const socket = world.sockets.last();
    socket.receive({ type: "fetch_range", request_id: 1, offset: 0, length: 4 });
    await settle();
    assert.deepEqual(fetched, [], "первый запрос ждёт правило");
    socket.receive({ type: "fetch_range", request_id: 2, offset: 0, length: 4 });
    await settle();
    assert.deepEqual(fetched, [LOCAL_URL], "вытесненный запрос не держит очередь");
    const finished = socket.sent.filter((payload): payload is string => typeof payload === "string").map((text) => JSON.parse(text) as Record<string, unknown>).filter((message) => message["type"] === "range_fetch_finished");
    assert.deepEqual(finished.map((message) => message["request_id"]), [1, 2]);
  });

  it("перенаправление с публичного файла на локальный адрес: ни байта плееру, итог network_error", async () => {
    const calls: string[] = [];
    const world = worldWith(PUBLIC_FILE_VIDEO, {}, (url, init) => {
      calls.push(url);
      return Promise.resolve(fileResponse(LOCAL_URL, init));
    });
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "file_source", url: PUBLIC_URL, referrer_origin: null, referrer_policy: "no-referrer" });
    const socket = world.sockets.last();
    socket.receive({ type: "fetch_range", request_id: 1, offset: 0, length: 4 });
    await settle();
    assert.ok(!socket.sent.some((payload) => typeof payload !== "string"), "ни одной рамки байтов");
    const finished = socket.sent.filter((payload): payload is string => typeof payload === "string").map((text) => JSON.parse(text) as Record<string, unknown>).find((message) => message["type"] === "range_fetch_finished");
    assert.deepEqual(finished?.["outcome"], { kind: "failed", failure: "network_error" });
    socket.receive({ type: "fetch_range", request_id: 2, offset: 0, length: 4 });
    await settle();
    assert.equal(calls.length, 1, "повтор плеера в сеть больше не идёт");
  });
});

describe("Referer закачки в сеансе (решение 202)", () => {
  it("запрос файла идёт под правилом с сайтом страницы, после запроса правила нет", async () => {
    let ruleDuringFetch: unknown;
    const world = worldWith(PUBLIC_FILE_VIDEO, {}, (url, init) => {
      const rule = world.sessionRules.added.at(-1);
      ruleDuringFetch = rule?.action.type === "modifyHeaders" ? rule.action.requestHeaders[0]?.value : undefined;
      return Promise.resolve(fileResponse(url, init));
    });
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "file_source", url: PUBLIC_URL, referrer_origin: "https://video.example.com", referrer_policy: "strict-origin-when-cross-origin" });
    world.sockets.last().receive({ type: "fetch_range", request_id: 1, offset: 0, length: 4 });
    await settle();
    assert.equal(ruleDuringFetch, "https://video.example.com/");
    assert.equal(world.sessionRules.requestRuleCount(), 0);
  });

  it("no-referrer на странице и <meta>-политика доходят до правила: same-origin на чужом файле правила не ставит", async () => {
    const world = worldWith(PUBLIC_FILE_VIDEO, {}, (url, init) => Promise.resolve(fileResponse(url, init)));
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "file_source", url: PUBLIC_URL, referrer_origin: "https://video.example.com", referrer_policy: "same-origin" });
    world.sockets.last().receive({ type: "fetch_range", request_id: 1, offset: 0, length: 4 });
    await settle();
    assert.equal(world.sessionRules.addedRequestRules().length, 0);
  });

  it("referrer_origin:null — правила нет вовсе", async () => {
    const world = worldWith(PUBLIC_FILE_VIDEO, {}, (url, init) => Promise.resolve(fileResponse(url, init)));
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "file_source", url: PUBLIC_URL, referrer_origin: null, referrer_policy: "no-referrer" });
    world.sockets.last().receive({ type: "fetch_range", request_id: 1, offset: 0, length: 4 });
    await settle();
    assert.equal(world.sessionRules.addedRequestRules().length, 0);
  });

  it("конец сеанса посреди запроса: запрос оборван, правило снято", async () => {
    let requestSignal: AbortSignal | undefined;
    const world = worldWith(PUBLIC_FILE_VIDEO, {}, (_url, init) => {
      requestSignal = init.signal;
      return new Promise((_resolve, reject) => init.signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError"))));
    });
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "file_source", url: PUBLIC_URL, referrer_origin: "https://video.example.com", referrer_policy: "strict-origin-when-cross-origin" });
    world.sockets.last().receive({ type: "fetch_range", request_id: 1, offset: 0, length: 4 });
    await settle();
    assert.equal(world.sessionRules.requestRuleCount(), 1, "запрос идёт, правило стоит");
    world.framePorts.last().deliver({ kind: "page_flood", over_budget: "mse" });
    await settle();
    assert.equal(requestSignal?.aborted, true);
    assert.equal(world.sessionRules.requestRuleCount(), 0);
  });
});

describe("команда с согласием", () => {
  const identity = { extensionId: "own", extensionBaseUrl: "chrome-extension://own/" };
  const ownPage = { id: "own", url: "chrome-extension://own/popup.html" };

  it("разбор: поле необязательно, значение только одно, чужие поля — не команда", () => {
    assert.deepEqual(parseHandOffTabRequest({ kind: "hand_off_tab", tab_id: 3 }), { kind: "hand_off_tab", tab_id: 3 });
    assert.deepEqual(parseHandOffTabRequest({ kind: "hand_off_tab", tab_id: 3, consented_local_host: "192.168.1.5" }), { kind: "hand_off_tab", tab_id: 3, consented_local_host: "192.168.1.5" });
    assert.equal(parseHandOffTabRequest({ kind: "hand_off_tab", tab_id: 3, consented_local_host: "" }), undefined);
    assert.equal(parseHandOffTabRequest({ kind: "hand_off_tab", tab_id: 3, consented_local_host: "a b" }), undefined);
    assert.equal(parseHandOffTabRequest({ kind: "hand_off_tab", tab_id: 3, consented_local_host: true }), undefined);
    assert.equal(parseHandOffTabRequest({ kind: "hand_off_tab", tab_id: 3, local_address: "allowed" }), undefined, "старая форма без хоста больше не согласие");
    assert.equal(parseHandOffTabRequest({ kind: "hand_off_tab", tab_id: 3, consented_local_host: "192.168.1.5", extra: 1 }), undefined);
  });

  it("согласие доходит до handOffTab; без поля — согласия нет", async () => {
    const seen: LocalAddressConsent[] = [];
    const listener = createHandOffCommandListener(identity, (_tabId, consent) => {
      seen.push(consent);
      return Promise.resolve({ kind: "no_video" } as HandOffTabOutcome);
    });
    listener({ kind: "hand_off_tab", tab_id: 3, consented_local_host: "192.168.1.5" }, ownPage, () => {});
    listener({ kind: "hand_off_tab", tab_id: 3 }, ownPage, () => {});
    await flushPromises();
    assert.deepEqual(seen, [{ kind: "given_for", host: "192.168.1.5" }, { kind: "absent" }]);
  });

  it("контент-скрипт согласие поставить не может: команда не принята", () => {
    const seen: LocalAddressConsent[] = [];
    const listener = createHandOffCommandListener(identity, (_tabId, consent) => {
      seen.push(consent);
      return Promise.resolve({ kind: "no_video" } as HandOffTabOutcome);
    });
    const handled = listener({ kind: "hand_off_tab", tab_id: 3, consented_local_host: "192.168.1.5" }, { id: "own", url: "https://evil.example/" }, () => {});
    assert.equal(handled, false);
    assert.deepEqual(seen, []);
  });
});
