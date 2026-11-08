// Правило «сирота или tab_gone» и память возврата страницы (сессия 11b2).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createPageReturnMemory, type ReturnablePage } from "../../../src/background/handoff/page-return/page-return-memory.ts";
import { decidePageLeave } from "../../../src/background/handoff/page-return/page-return-rules.ts";
import { PROTOCOL_VERSION_1_3, PROTOCOL_VERSION_1_4 } from "../../../src/protocol/version.ts";
import type { PageReturnTarget } from "../../../src/shared/handoff-contract.ts";

const PAGE: ReturnablePage = { url: "https://video.example/watch?v=7", windowId: 3, incognito: false };
const FILM: PageReturnTarget = { kind: "recorded", duration_s: 1_200, start_s: 61 };
const FACTS = { reason: "closed" as const, agreedProtocol: PROTOCOL_VERSION_1_4, page: PAGE, target: FILM };

describe("уход страницы: сирота или tab_gone", () => {
  it("1.4, вкладку закрыли или ушли, адрес http(s), фильм известен — сирота с этой страницей и целью", () => {
    assert.deepEqual(decidePageLeave(FACTS), { kind: "orphan", page: PAGE, target: FILM });
    assert.equal(decidePageLeave({ ...FACTS, reason: "navigated_away" }).kind, "orphan");
    assert.equal(decidePageLeave({ ...FACTS, page: { ...PAGE, url: "http://video.example/a" } }).kind, "orphan");
  });

  it("плеер до 1.4 — прежний tab_gone: `page_left` для него нарушение", () => {
    assert.deepEqual(decidePageLeave({ ...FACTS, agreedProtocol: PROTOCOL_VERSION_1_3 }), { kind: "tab_gone" });
  });

  it("сломался плеер сайта или браузер закрывается — tab_gone: та же страница снова сломалась бы", () => {
    assert.equal(decidePageLeave({ ...FACTS, reason: "page_player_failed" }).kind, "tab_gone");
    assert.equal(decidePageLeave({ ...FACTS, reason: "browser_closing" }).kind, "tab_gone");
  });

  it("адрес не http(s), страница не описана или фильм неизвестен (реклама на экране) — tab_gone", () => {
    for (const url of ["file:///home/video.mp4", "chrome://newtab/", "about:blank", "не адрес"]) {
      assert.equal(decidePageLeave({ ...FACTS, page: { ...PAGE, url } }).kind, "tab_gone", url);
    }
    assert.equal(decidePageLeave({ ...FACTS, page: undefined }).kind, "tab_gone");
    assert.equal(decidePageLeave({ ...FACTS, target: undefined }).kind, "tab_gone");
  });
});

describe("память возврата", () => {
  it("цель — длина фильма и ПОСЛЕДНЯЯ секунда часов плеера", () => {
    const memory = createPageReturnMemory();
    memory.noteVideoInPlayer({ role: "film", durationS: 1_200 });
    memory.notePlayerSecond(60_000_000);
    memory.notePlayerSecond(61_500_000);
    assert.deepEqual(memory.returnTarget(), { kind: "recorded", duration_s: 1_200, start_s: 61.5 });
  });

  it("часы «доиграно» за краем ролика не уводят перемотку за конец", () => {
    const memory = createPageReturnMemory();
    memory.noteVideoInPlayer({ role: "film", durationS: 30 });
    memory.notePlayerSecond(31_000_000);
    assert.deepEqual(memory.returnTarget(), { kind: "recorded", duration_s: 30, start_s: 30 });
  });

  it("реклама фильма не называет: цели нет, пока на экране она; эфир — без длины и секунды", () => {
    const memory = createPageReturnMemory();
    assert.equal(memory.returnTarget(), undefined, "видео ещё не вставало");
    memory.noteVideoInPlayer({ role: "film", durationS: 600 });
    memory.noteVideoInPlayer({ role: "ad", durationS: 15 });
    assert.equal(memory.returnTarget(), undefined);
    memory.noteVideoInPlayer({ role: "film", durationS: null });
    assert.deepEqual(memory.returnTarget(), { kind: "live" });
  });

  it("страница помнит последнее место; `undefined` — браузер её не описал", () => {
    const memory = createPageReturnMemory();
    memory.notePage(PAGE);
    assert.deepEqual(memory.page(), PAGE);
    memory.notePage(undefined);
    assert.equal(memory.page(), undefined);
  });
});
