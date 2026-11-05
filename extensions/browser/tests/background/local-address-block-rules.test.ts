// Сетевой заслон локальных адресов (решение 199): совпадение с проверкой адреса и поведение на модели
// браузера, которая разбирает правила и перенаправления так же, как настоящий (порядок приоритетов —
// по документации declarativeNetRequest, живое подтверждение — сценарий стенда `local-address`).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createFileReferrerRules } from "../../src/background/handoff/file-referrer-rule.ts";
import { isLocalHost, type FileAddressRule } from "../../src/background/handoff/file-address-guard.ts";
import { createLocalAddressBlock, LOCAL_ADDRESS_URL_FILTERS, localAddressBlockRules } from "../../src/background/handoff/local-address-block-rules.ts";
import type { RangeFetch, RangeFetchInit } from "../../src/background/handoff/range-fetcher.ts";
import { createSessionRules, SESSION_RULE_PRIORITY, type SessionRule, type SessionRulesApi } from "../../src/background/handoff/session-rules.ts";
import { createFakeSessionRules, EXTENSION_HOST } from "./handoff-world.ts";
import { LOCAL_ADDRESS_HOST_SAMPLES, PUBLIC_ADDRESS_HOST_SAMPLES_OUTSIDE_BLOCK, PUBLIC_ADDRESS_HOST_SAMPLES_UNDER_BLOCK } from "./local-address-samples.ts";
import { createFakeClock, flushPromises } from "./support.ts";

const blocksUrl = (url: string): boolean => LOCAL_ADDRESS_URL_FILTERS.some((filter) => new RegExp(filter).test(new URL(url).href));

/**
 * Самый длинный фильтр, символов. Настоящий предел Chrome — 2 КБ скомпилированной программы RE2; без браузера
 * его не проверить, а эта длина с символами без отрицаний проходит RE2 с параметрами Chrome с запасом.
 */
const MAX_FILTER_LENGTH = 130;

const INIT: RangeFetchInit = { headers: { Range: "bytes=0-9" }, credentials: "include", cache: "no-store", redirect: "follow", signal: new AbortController().signal };
const PUBLIC_RULE: FileAddressRule = { pageHost: "video.example.com", setting: "block", consentedHost: null };
const CONSENT_RULE: FileAddressRule = { pageHost: "video.example.com", setting: "ask", consentedHost: "192.168.1.5" };
const LOCAL_PAGE_RULE: FileAddressRule = { pageHost: "nas.local", setting: "block", consentedHost: null };
const REDIRECT_START = "https://cdn.example/movie.mp4";

describe("фильтры заслона совпадают с проверкой адреса", () => {
  for (const url of LOCAL_ADDRESS_HOST_SAMPLES) it(`закрыт: ${url}`, () => assert.equal(blocksUrl(url), true, new URL(url).href));
  for (const url of PUBLIC_ADDRESS_HOST_SAMPLES_OUTSIDE_BLOCK) it(`открыт: ${url}`, () => assert.equal(blocksUrl(url), false, new URL(url).href));
  for (const url of PUBLIC_ADDRESS_HOST_SAMPLES_UNDER_BLOCK) {
    it(`закрыт намеренно шире проверки: ${url}`, () => {
      assert.equal(blocksUrl(url), true);
      assert.equal(isLocalHost(new URL(url).hostname), false);
    });
  }

  it("хост проверяется целиком: локальное имя в пути и запросе заслон не включает", () => {
    assert.equal(blocksUrl("https://cdn.example/192.168.1.1/nas.local"), false);
    assert.equal(blocksUrl("https://cdn.example/?host=localhost"), false);
    assert.equal(blocksUrl("http://192.168.1.1:8080/reboot"), true);
    assert.equal(blocksUrl("http://192.168.1.1?x"), true);
  });

  it("фильтры укладываются в предел Chrome: одна слишком сложная регулярка отвергла бы весь заслон", () => {
    for (const filter of LOCAL_ADDRESS_URL_FILTERS) {
      assert.ok(filter.length <= MAX_FILTER_LENGTH, `${filter.length}: ${filter}`);
      assert.ok(!filter.includes("[^"), `отрицание разворачивается в десятки диапазонов UTF-8: ${filter}`);
      assert.ok(!/\(\?[=!<]|\\[1-9]/.test(filter), filter);
      assert.ok(/^[\x20-\x7e]+$/.test(filter), filter);
    }
  });
});

describe("форма постоянных правил", () => {
  const rules = localAddressBlockRules(EXTENSION_HOST);

  it("только `fetch` самого экстеншена: WebSocket к плееру и запросы вкладок и чужих service worker вне правила", () => {
    assert.ok(rules.length > 0);
    for (const rule of rules) {
      assert.deepEqual(rule.action, { type: "block" });
      assert.deepEqual(rule.condition.resourceTypes, ["xmlhttprequest"]);
      assert.deepEqual(rule.condition.tabIds, [-1]);
      assert.deepEqual(rule.condition.initiatorDomains, [EXTENSION_HOST]);
      assert.equal(rule.condition.isUrlFilterCaseSensitive, true, "свёртка регистра раздувает программу RE2");
    }
  });

  it("ступени приоритета: разрешение выше заслона, Referer строго выше разрешения", () => {
    assert.ok(SESSION_RULE_PRIORITY.allow > SESSION_RULE_PRIORITY.block);
    assert.ok(SESSION_RULE_PRIORITY.header > SESSION_RULE_PRIORITY.allow, "иначе Chrome отбросит заголовок на разрешённом запросе");
  });
});

/** Модель браузера: приоритет решает между `block` и `allow`, на каждом шаге перенаправления заново. */
function evaluatesAsBlocked(rules: Iterable<SessionRule | undefined>, url: string): boolean {
  let winner: SessionRule | undefined;
  for (const rule of rules) {
    if (rule === undefined || rule.action.type === "modifyHeaders") continue;
    const { regexFilter } = rule.condition;
    if (regexFilter !== undefined && !new RegExp(regexFilter).test(url)) continue;
    if (winner === undefined || rule.priority > winner.priority) winner = rule;
  }
  return winner?.action.type === "block";
}

/** Мир: фон качает `REDIRECT_START`, сервер отвечает 302 на `target`, браузер применяет правила. */
function redirectWorld(target: string) {
  const browser = createFakeSessionRules();
  const requestRules = createSessionRules({ api: browser.api, scheduler: createFakeClock().scheduler, permanentRules: localAddressBlockRules(EXTENSION_HOST), reportFailureOnce: () => {} });
  const reached: string[] = [];
  const network: RangeFetch = async (url) => {
    for (const hop of [url, target]) {
      if (evaluatesAsBlocked(browser.rules.values(), hop)) throw new TypeError("Failed to fetch");
      reached.push(hop);
    }
    return new Response(null, { status: 206 }) as never;
  };
  return { browser, reached, block: createLocalAddressBlock(requestRules, EXTENSION_HOST), network, requestRules };
}

describe("перенаправление на локальный адрес", () => {
  it("публичный файл с 302 на роутер: запрос на роутер не уходит вообще", async () => {
    const world = redirectWorld("http://192.168.1.1/reboot");
    await flushPromises();
    await assert.rejects(world.block.around(world.network, PUBLIC_RULE)(REDIRECT_START, INIT));
    assert.deepEqual(world.reached, [REDIRECT_START], "сервер роутера не видел ни одного запроса");
  });

  it("согласие на один хост не открывает другой", async () => {
    const world = redirectWorld("http://192.168.1.1/reboot");
    await flushPromises();
    await assert.rejects(world.block.around(world.network, CONSENT_RULE)(REDIRECT_START, INIT));
    assert.deepEqual(world.reached, [REDIRECT_START]);
  });

  it("согласованный хост проходит, пока идёт запрос, а после запроса разрешения нет", async () => {
    const world = redirectWorld("http://192.168.1.5:8096/movie.mp4");
    await flushPromises();
    await world.block.around(world.network, CONSENT_RULE)(REDIRECT_START, INIT);
    assert.equal(world.reached.length, 2);
    await flushPromises();
    assert.equal(evaluatesAsBlocked(world.browser.rules.values(), "http://192.168.1.5:8096/movie.mp4"), true, "заслон снова закрыт");
  });

  it("локальная страница ходит в локальную сеть как обычно", async () => {
    const world = redirectWorld("http://192.168.1.1/movie.mp4");
    await flushPromises();
    await world.block.around(world.network, LOCAL_PAGE_RULE)("http://nas.local/movie.mp4", INIT);
    assert.equal(world.reached.length, 2);
  });

  it("сеанс без локального доступа не трогает API правил: обычное видео не платит за заслон", async () => {
    const world = redirectWorld("https://cdn2.example/movie.mp4");
    await flushPromises();
    const addedBefore = world.browser.added.length;
    await world.block.around(world.network, PUBLIC_RULE)(REDIRECT_START, INIT);
    assert.equal(world.browser.added.length, addedBefore);
    assert.equal(world.reached.length, 2);
  });

  it("разбуженный фон: первый запрос идёт только после того, как заслон встал", async () => {
    const browser = createFakeSessionRules();
    let releaseStart: () => void = () => {};
    const slowStart: SessionRulesApi = {
      ...browser.api,
      updateSessionRules: async (options) => {
        await new Promise<void>((resolve) => (releaseStart = resolve));
        await browser.api.updateSessionRules(options);
      },
    };
    const requestRules = createSessionRules({ api: slowStart, scheduler: createFakeClock().scheduler, permanentRules: localAddressBlockRules(EXTENSION_HOST), reportFailureOnce: () => {} });
    const fetched: string[] = [];
    const network: RangeFetch = async (url) => {
      fetched.push(url);
      return new Response(null, { status: 206 }) as never;
    };
    const request = createLocalAddressBlock(requestRules, EXTENSION_HOST).around(network, PUBLIC_RULE)(REDIRECT_START, INIT);
    await flushPromises();
    assert.deepEqual(fetched, [], "заслона ещё нет: запрос ждёт");
    releaseStart();
    await request;
    assert.deepEqual(fetched, [REDIRECT_START]);
  });

  it("Referer на разрешённом локальном запросе стоит строго выше разрешения", async () => {
    const world = redirectWorld("http://192.168.1.5/movie.mp4");
    await flushPromises();
    const referrer = createFileReferrerRules(world.requestRules);
    const network = referrer.around(world.block.around(world.network, LOCAL_PAGE_RULE), { origin: "http://nas.local", policy: "strict-origin-when-cross-origin" });
    await network("http://nas.local/movie.mp4", INIT);
    const header = world.browser.added.find((rule) => rule.action.type === "modifyHeaders");
    const allow = world.browser.added.find((rule) => rule.action.type === "allow");
    assert.ok(header !== undefined && allow !== undefined);
    assert.ok(header.priority > allow.priority);
  });
});
