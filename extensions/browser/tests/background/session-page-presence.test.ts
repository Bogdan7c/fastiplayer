// Присутствие страницы сеанса (вынесено из сеанса в 11b2): вкладка страницы и вопросы браузеру о её уходе.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createPagePresence, type FrameLossAnswer } from "../../src/background/handoff/session-page-presence.ts";
import type { TabGoneReason } from "../../src/protocol/index.ts";
import { flushPromises } from "./support.ts";

/** Браузер, который отвечает, когда велит сценарий. */
function controllableBrowser() {
  const exists: { tabId: number; answer: (alive: boolean) => void }[] = [];
  const described: { tabId: number; answer: (url: string | undefined) => void }[] = [];
  return {
    exists,
    described,
    browser: {
      tabExists: (tabId: number) => new Promise<boolean>((resolve) => void exists.push({ tabId, answer: resolve })),
      describeTab: (tabId: number) =>
        new Promise<{ url?: string } | undefined>((resolve) => void described.push({ tabId, answer: (url) => resolve(url === undefined ? undefined : { url }) })),
    },
  };
}

describe("присутствие страницы", () => {
  it("порт главного документа умер: жива вкладка — ушли, нет — закрыли; один вопрос за раз", async () => {
    const control = controllableBrowser();
    const presence = createPagePresence({ tabId: 12, tabUrl: "https://a.example/x" }, control.browser);
    const answers: TabGoneReason[] = [];
    presence.askWhyPageLeft((reason) => void answers.push(reason));
    presence.askWhyPageLeft((reason) => void answers.push(reason));
    assert.equal(control.exists.length, 1, "второй вопрос не задан, пока первый в пути");
    control.exists[0]?.answer(true);
    await flushPromises();
    assert.deepEqual(answers, ["navigated_away"]);
    presence.markRemoved();
    presence.askWhyPageLeft((reason) => void answers.push(reason));
    control.exists[1]?.answer(true);
    await flushPromises();
    assert.deepEqual(answers, ["navigated_away", "closed"], "onRemoved пришёл, пока спрашивали: закрыли");
  });

  it("порт рекламного фрейма умер: тот же адрес — страница на месте, другой — ушли, вкладки нет — закрыли", async () => {
    const control = controllableBrowser();
    const presence = createPagePresence({ tabId: 12, tabUrl: "https://a.example/x" }, control.browser);
    const answers: FrameLossAnswer[] = [];
    for (const url of ["https://a.example/x", "https://b.example/", undefined]) {
      presence.askWhetherPageStayed((answer) => void answers.push(answer));
      control.described.at(-1)?.answer(url);
      await flushPromises();
    }
    assert.deepEqual(answers, ["same_page", "navigated_away", "closed"]);
  });

  it("страница переехала в другую вкладку (возврат): ответ о старой вкладке не доходит, отметка «закрыта» снята", async () => {
    const control = controllableBrowser();
    const presence = createPagePresence({ tabId: 12, tabUrl: "https://a.example/x" }, control.browser);
    const answers: TabGoneReason[] = [];
    presence.markRemoved();
    presence.askWhyPageLeft((reason) => void answers.push(reason));
    presence.moveTo(31, "https://a.example/x");
    control.exists[0]?.answer(false);
    await flushPromises();
    assert.equal(answers.length, 0, "запоздалый ответ о вкладке 12 страницу в вкладке 31 не трогает");
    assert.equal(presence.tabId(), 31);
    assert.equal(presence.wasRemoved(), false);
    presence.askWhyPageLeft((reason) => void answers.push(reason));
    assert.equal(control.exists.at(-1)?.tabId, 31, "новый вопрос — о новой вкладке");
  });
});
