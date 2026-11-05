// Сессионные правила фона: старт, номера и установка, ограниченная сроком и обрывом запроса.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createSessionRules, FIRST_REQUEST_RULE_ID, RULE_INSTALL_TIMEOUT_MS, underRequestRule, type SessionRule, type SessionRuleDraft, type SessionRulesApi } from "../../src/background/handoff/session-rules.ts";
import { createFakeSessionRules } from "./handoff-world.ts";
import { createFakeClock, flushPromises } from "./support.ts";

const DRAFT: SessionRuleDraft = { priority: 1, action: { type: "allow" }, condition: { tabIds: [-1], resourceTypes: ["xmlhttprequest"] } };
const PERMANENT: SessionRule = { ...DRAFT, id: 1, action: { type: "block" } };

function setupRules(api: SessionRulesApi, permanentRules: readonly SessionRule[] = []) {
  const clock = createFakeClock();
  const failures: unknown[] = [];
  const rules = createSessionRules({ api, scheduler: clock.scheduler, permanentRules, reportFailureOnce: (error) => void failures.push(error) });
  return { clock, failures, rules };
}

/** API, у которого добавление правил зависает, пока тест его не отпустит. */
function hangingAdditions(api: SessionRulesApi) {
  const pending: (() => void)[] = [];
  const hanging: SessionRulesApi = {
    getSessionRules: api.getSessionRules,
    updateSessionRules: (options) => {
      if (options.addRules === undefined || options.addRules.length === 0) return api.updateSessionRules(options);
      return new Promise((resolve) => pending.push(() => void api.updateSessionRules(options).then(resolve)));
    },
  };
  return { hanging, pending };
}

describe("старт фона", () => {
  it("правила прошлой жизни сняты, постоянные поставлены, одним обновлением", async () => {
    const browser = createFakeSessionRules([1, 2, 1_077]);
    const calls: unknown[] = [];
    const counted: SessionRulesApi = {
      getSessionRules: browser.api.getSessionRules,
      updateSessionRules: (options) => {
        calls.push(options);
        return browser.api.updateSessionRules(options);
      },
    };
    setupRules(counted, [PERMANENT]);
    await flushPromises();
    assert.deepEqual([...browser.rules.keys()], [PERMANENT.id]);
    assert.equal(calls.length, 1, "окна без заслона нет: снятие и установка атомарны");
  });

  it("правило на запрос ждёт старта и получает номер выше постоянных", async () => {
    const browser = createFakeSessionRules();
    const { rules } = setupRules(browser.api, [PERMANENT]);
    const id = await rules.install(DRAFT, new AbortController().signal);
    assert.equal(id, FIRST_REQUEST_RULE_ID);
    assert.deepEqual([...browser.rules.keys()], [PERMANENT.id, FIRST_REQUEST_RULE_ID]);
  });

  it("список старых правил не прочитан: постоянные всё равно ставятся, сбой виден", async () => {
    const browser = createFakeSessionRules();
    const { failures } = setupRules({ ...browser.api, getSessionRules: () => Promise.reject(new Error("no list")) }, [PERMANENT]);
    await flushPromises();
    assert.deepEqual([...browser.rules.keys()], [PERMANENT.id]);
    assert.equal(failures.length, 1);
  });
});

describe("установка правила на запрос", () => {
  it("зависший API не держит запрос: после срока установка кончается, сбой виден", async () => {
    const browser = createFakeSessionRules();
    const { hanging } = hangingAdditions(browser.api);
    const { clock, failures, rules } = setupRules(hanging);
    const install = rules.install(DRAFT, new AbortController().signal);
    await flushPromises();
    clock.advance(RULE_INSTALL_TIMEOUT_MS);
    assert.equal(await install, undefined);
    assert.equal(failures.length, 1);
  });

  it("обрыв запроса кончает установку сразу, без ожидания срока", async () => {
    const browser = createFakeSessionRules();
    const { hanging } = hangingAdditions(browser.api);
    const { rules, failures } = setupRules(hanging);
    const abort = new AbortController();
    const install = rules.install(DRAFT, abort.signal);
    await flushPromises();
    abort.abort();
    assert.equal(await install, undefined);
    assert.equal(failures.length, 0, "обрыв — не сбой API");
  });

  it("правило, вставшее уже после отказа от него, снимается: чужому запросу оно не достанется", async () => {
    const browser = createFakeSessionRules();
    const { hanging, pending } = hangingAdditions(browser.api);
    const { rules } = setupRules(hanging);
    const abort = new AbortController();
    const install = rules.install(DRAFT, abort.signal);
    await flushPromises();
    abort.abort();
    await install;
    pending.shift()?.();
    await flushPromises();
    await flushPromises();
    assert.equal(browser.added.length, 1, "API всё же поставил правило");
    assert.equal(browser.rules.size, 0, "и оно снято");
  });

  it("уже оборванный запрос к API не идёт", async () => {
    const browser = createFakeSessionRules();
    const { rules } = setupRules(browser.api);
    const abort = new AbortController();
    abort.abort();
    assert.equal(await rules.install(DRAFT, abort.signal), undefined);
    assert.equal(browser.added.length, 0);
  });
});

describe("запрос под правилом", () => {
  it("зависший API правил: оборванный запрос в сеть не идёт, а его промис кончается", async () => {
    const browser = createFakeSessionRules();
    const { hanging } = hangingAdditions(browser.api);
    const { rules } = setupRules(hanging);
    const abort = new AbortController();
    let sent = 0;
    const request = underRequestRule(rules, DRAFT, abort.signal, () => {
      sent += 1;
      return Promise.resolve();
    });
    await flushPromises();
    abort.abort();
    await assert.rejects(request);
    assert.equal(sent, 0);
  });

  it("срок вышел: запрос всё равно уходит, без правила", async () => {
    const browser = createFakeSessionRules();
    const { hanging } = hangingAdditions(browser.api);
    const { clock, rules } = setupRules(hanging);
    let sent = 0;
    const request = underRequestRule(rules, DRAFT, new AbortController().signal, () => {
      sent += 1;
      return Promise.resolve();
    });
    await flushPromises();
    clock.advance(RULE_INSTALL_TIMEOUT_MS);
    await request;
    assert.equal(sent, 1);
  });
});
