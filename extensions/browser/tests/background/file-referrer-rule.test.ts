// Referer закачки файла (решение 202): правило браузера на один запрос фона.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createFileReferrerRules, exactUrlFilter } from "../../src/background/handoff/file-referrer-rule.ts";
import type { RangeFetch, RangeFetchInit, RangeFetchResponse } from "../../src/background/handoff/range-fetcher.ts";
import { createSessionRules, NO_TAB_ID, type SessionRule, type SessionRulesApi } from "../../src/background/handoff/session-rules.ts";
import type { FileReferrer } from "../../src/shared/referrer-policy.ts";
import { createFakeSessionRules } from "./handoff-world.ts";
import { createFakeClock, flushPromises } from "./support.ts";

const FILE_URL = "https://cdn.example/movie.mp4?sig=abc";
const SITE_ORIGIN = "https://site.example";
const SITE: FileReferrer = { origin: SITE_ORIGIN, policy: "strict-origin-when-cross-origin" };
const INIT: RangeFetchInit = { headers: { Range: "bytes=0-9" }, credentials: "include", cache: "no-store", redirect: "follow", signal: new AbortController().signal };
const RESPONSE = new Response(null, { status: 206 }) as RangeFetchResponse;

function setupRules(api: SessionRulesApi = createFakeSessionRules().api) {
  const failures: unknown[] = [];
  const requestRules = createSessionRules({ api, scheduler: createFakeClock().scheduler, permanentRules: [], reportFailureOnce: (error) => void failures.push(error) });
  const rules = createFileReferrerRules(requestRules);
  return { rules, failures };
}

describe("правило Referer закачки", () => {
  it("правило стоит ровно на время запроса: значение — сайт плюс /, адрес точный, только запросы фона", async () => {
    const browser = createFakeSessionRules();
    const { rules } = setupRules(browser.api);
    let seenDuringFetch: readonly SessionRule[] = [];
    const network: RangeFetch = () => {
      seenDuringFetch = [...browser.rules.values()].flatMap((rule) => (rule === undefined ? [] : [rule]));
      return Promise.resolve(RESPONSE);
    };
    const response = await rules.around(network, SITE)(FILE_URL, INIT);
    await flushPromises();
    assert.equal(response, RESPONSE);
    assert.equal(seenDuringFetch.length, 1, "к моменту `fetch` правило уже стоит");
    const [rule] = seenDuringFetch;
    assert.deepEqual(rule?.action, { type: "modifyHeaders", requestHeaders: [{ header: "Referer", operation: "set", value: `${SITE_ORIGIN}/` }] });
    assert.equal(rule?.condition.urlFilter, `|${FILE_URL}|`);
    assert.deepEqual(rule?.condition.resourceTypes, ["xmlhttprequest", "media", "other"]);
    assert.equal(browser.rules.size, 0, "после запроса правила нет");
  });

  it("tabIds:[-1] обязателен: без него правило протекает на запросы страницы", async () => {
    const browser = createFakeSessionRules();
    const { rules } = setupRules(browser.api);
    await rules.around(() => Promise.resolve(RESPONSE), SITE)(FILE_URL, INIT);
    assert.equal(NO_TAB_ID, -1);
    assert.deepEqual(browser.added[0]?.condition.tabIds, [-1]);
  });

  it("запрос упал или оборван — правило всё равно снято, ошибка уходит вызывающему", async () => {
    const browser = createFakeSessionRules();
    const { rules } = setupRules(browser.api);
    const aborted = new DOMException("aborted", "AbortError");
    await assert.rejects(rules.around(() => Promise.reject(aborted), SITE)(FILE_URL, INIT), aborted);
    await flushPromises();
    assert.equal(browser.added.length, 1);
    assert.equal(browser.rules.size, 0);
  });

  it("перекрывающиеся закачки: у каждой свой номер и своё значение, конец одной не трогает другую", async () => {
    const browser = createFakeSessionRules();
    const { rules } = setupRules(browser.api);
    let releaseFirst: (response: RangeFetchResponse) => void = () => {};
    const slow: RangeFetch = () => new Promise((resolve) => (releaseFirst = resolve));
    const first = rules.around(slow, SITE)(FILE_URL, INIT);
    await flushPromises();
    await rules.around(() => Promise.resolve(RESPONSE), { origin: "https://other.example", policy: "strict-origin-when-cross-origin" })("https://cdn.example/other.mp4", INIT);
    await flushPromises();
    assert.deepEqual([...browser.rules.keys()], [browser.added[0]?.id], "снято только правило второй закачки");
    assert.notEqual(browser.added[0]?.id, browser.added[1]?.id);
    releaseFirst(RESPONSE);
    await first;
    await flushPromises();
    assert.equal(browser.rules.size, 0);
  });

  it("API правил нет или оно падает: закачка идёт без Referer, сбой в журнале один раз", async () => {
    const broken: SessionRulesApi = {
      updateSessionRules: () => Promise.reject(new Error("no such api")),
      getSessionRules: () => Promise.reject(new Error("no such api")),
    };
    const { rules, failures } = setupRules(broken);
    const fetched: string[] = [];
    const network: RangeFetch = (url) => {
      fetched.push(url);
      return Promise.resolve(RESPONSE);
    };
    await rules.around(network, SITE)(FILE_URL, INIT);
    await rules.around(network, SITE)(FILE_URL, INIT);
    assert.deepEqual(fetched, [FILE_URL, FILE_URL], "обе закачки ушли");
    assert.equal(failures.length, 1);
  });

  it("сбой установки вперёд снятия: снимать нечего, лишнего вызова нет", async () => {
    const browser = createFakeSessionRules();
    let calls = 0;
    const flaky: SessionRulesApi = {
      getSessionRules: browser.api.getSessionRules,
      updateSessionRules: (options) => {
        calls += 1;
        return options.addRules === undefined ? browser.api.updateSessionRules(options) : Promise.reject(new Error("quota"));
      },
    };
    const { rules } = setupRules(flaky);
    await rules.around(() => Promise.resolve(RESPONSE), SITE)(FILE_URL, INIT);
    await flushPromises();
    assert.equal(calls, 1, "только неудачная установка");
  });
});

describe("значение и фильтр правила", () => {
  it("браузер не послал бы Referer: правила нет, запрос уходит без него", async () => {
    const browser = createFakeSessionRules();
    const { rules } = setupRules(browser.api);
    await rules.around(() => Promise.resolve(RESPONSE), SITE)("http://cdn.example/a.mp4", INIT);
    assert.equal(browser.added.length, 0);
  });

  it("значение считается на каждый адрес: same-origin не уносит сайт на чужой адрес после перенаправления", async () => {
    const browser = createFakeSessionRules();
    const { rules } = setupRules(browser.api);
    const network = rules.around(() => Promise.resolve(RESPONSE), { origin: SITE_ORIGIN, policy: "same-origin" });
    await network(`${SITE_ORIGIN}/a.mp4`, INIT);
    await network("https://cdn.example/a.mp4", INIT);
    assert.equal(browser.added.length, 1, "правило только на свой сайт");
    assert.equal(browser.added[0]?.condition.urlFilter, `|${SITE_ORIGIN}/a.mp4|`);
  });

  it("фрагмент адреса в фильтр не входит: в запрос он не уходит, и правило бы не совпало", () => {
    assert.equal(exactUrlFilter("https://cdn.example/a.mp4#t=10"), "|https://cdn.example/a.mp4|");
    assert.equal(exactUrlFilter("https://cdn.example/a.mp4?x=1"), "|https://cdn.example/a.mp4?x=1|");
  });
});
