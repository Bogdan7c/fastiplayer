// Итог входа без попапа (сессия 10c): тексты плашки и репортёр (плашка + вкладочный значок).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { describeEntryResult, type EntryResult } from "../../src/background/entries/entry-report-text.ts";
import { createEntryReporter, type EntryBadge, type EntryBanner } from "../../src/background/entries/entry-reporter.ts";
import { describeOutcome } from "../../src/popup/outcome-text.ts";
import type { EntryReport } from "../../src/shared/entry-contract.ts";
import type { HandOffTabOutcome, UnsupportedMediaForm } from "../../src/shared/handoff-contract.ts";

const FORMS: readonly UnsupportedMediaForm[] = ["native_hls", "blob_file", "data_url", "media_stream", "other_scheme", "untracked_worker_media_source", "started_before_script"];

/** Каждый вид исхода команды передачи (и обе формы удачи) плюс два исхода, которых у команды нет. */
const ALL_RESULTS: readonly EntryResult[] = [
  { kind: "outcome", outcome: { kind: "started", player_window: "new" } },
  { kind: "outcome", outcome: { kind: "started", player_window: "already_open" } },
  { kind: "outcome", outcome: { kind: "no_video" } },
  { kind: "outcome", outcome: { kind: "not_supported_yet", form: "native_hls" } },
  { kind: "outcome", outcome: { kind: "protected" } },
  { kind: "outcome", outcome: { kind: "video_gone" } },
  { kind: "outcome", outcome: { kind: "local_address_refused" } },
  { kind: "outcome", outcome: { kind: "local_address_needs_consent", host: "nas.local" } },
  { kind: "outcome", outcome: { kind: "player_unavailable", reason: "player_not_responding" } },
  { kind: "outcome", outcome: { kind: "failed", reason: "tab_not_scriptable:no frame accepted the injection" } },
  { kind: "link_no_video_started" },
  { kind: "link_not_opened" },
  { kind: "outcome", outcome: { kind: "reload_needed", reason: "script_missing" } },
  { kind: "late_press_play" },
  { kind: "late_no_video_started" },
  { kind: "late_reload_failed" },
  { kind: "late_started_from_site_position" },
];

const textOf = (result: EntryResult): string => {
  const report = describeEntryResult(result);
  return [report.headline, ...report.details].join(" ");
};

describe("тексты плашки", () => {
  it("каждый вариант исхода даёт непустой заголовок и своё имя случая", () => {
    const reports = ALL_RESULTS.map(describeEntryResult);
    for (const report of reports) assert.notEqual(report.headline, "", report.id);
    assert.equal(new Set(reports.map((report) => report.id)).size, ALL_RESULTS.length, "имена случаев не повторяются");
  });

  it("удача — success, подсказка — hint, всё остальное — refusal", () => {
    const expectedTone = (result: EntryResult): EntryReport["tone"] => {
      if (result.kind === "late_press_play") return "hint";
      if (result.kind === "late_started_from_site_position") return "success";
      return result.kind === "outcome" && result.outcome.kind === "started" ? "success" : "refusal";
    };
    assert.deepEqual(ALL_RESULTS.map((result) => describeEntryResult(result).tone), ALL_RESULTS.map(expectedTone));
  });

  it("плашка про перезагрузку называет кнопку на панели: сама она страницу не перезагружает", () => {
    const text = textOf({ kind: "outcome", outcome: { kind: "reload_needed", reason: "stream_started_before_script" } });
    assert.match(text, /started before Fastiplayer Bridge was ready/);
    assert.match(text, /toolbar/);
  });

  it("сайт не дал встать на секунду: человек узнаёт об этом", () => {
    assert.match(textOf({ kind: "late_started_from_site_position" }), /may not continue from the same second/);
  });

  it("started: новое окно — «Opened in Fastiplayer»; окно уже открыто — тот же совет, что в попапе (решение 216)", () => {
    assert.deepEqual(describeEntryResult({ kind: "outcome", outcome: { kind: "started", player_window: "new" } }), {
      id: "opened",
      tone: "success",
      headline: "Opened in Fastiplayer",
      details: [],
    });
    const popup = describeOutcome({ kind: "started", player_window: "already_open" });
    const banner = describeEntryResult({ kind: "outcome", outcome: { kind: "started", player_window: "already_open" } });
    assert.equal(banner.id, popup.id);
    assert.equal(banner.headline, popup.headline);
    assert.deepEqual(banner.details, popup.details);
  });

  it("no_video говорит, что делать; локальный адрес называет хост и кнопку на панели; плеер — про панель и статус", () => {
    assert.match(textOf({ kind: "outcome", outcome: { kind: "no_video" } }), /Press play on the page, then try again\./);
    const consent = textOf({ kind: "outcome", outcome: { kind: "local_address_needs_consent", host: "nas.local" } });
    assert.match(consent, /nas\.local/);
    assert.match(consent, /toolbar/);
    assert.match(textOf({ kind: "outcome", outcome: { kind: "player_unavailable", reason: "x" } }), /Fastiplayer did not take the video.*toolbar/);
  });

  it("not_supported_yet: слово формы то же, что в попапе — по одному источнику", () => {
    for (const form of FORMS) {
      const outcome: HandOffTabOutcome = { kind: "not_supported_yet", form };
      const popupWords = describeOutcome(outcome).details.join(" ");
      assert.ok(textOf({ kind: "outcome", outcome }).includes(popupWords), form);
    }
  });

  it("ссылка без видео: «No video started on this page» и следующий шаг; причина (reason) пользователю не показывается", () => {
    const report = describeEntryResult({ kind: "link_no_video_started" });
    assert.equal(report.headline, "No video started on this page");
    assert.match(report.details.join(" "), /Press play on the page to send it to Fastiplayer/);
    assert.ok(!textOf({ kind: "outcome", outcome: { kind: "failed", reason: "internal:secret" } }).includes("secret"));
  });
});

function setupReporter(options: { bannerFails?: boolean; badgeFails?: boolean } = {}) {
  const log: string[] = [];
  const banners: { tabId: number; report: EntryReport }[] = [];
  const notes: string[] = [];
  const banner: EntryBanner = {
    show: (tabId, report) => {
      banners.push({ tabId, report });
      return options.bannerFails === true ? Promise.reject(new Error("Could not establish connection")) : Promise.resolve();
    },
  };
  const badge: EntryBadge = {
    markProblem: (tabId) => {
      log.push(`mark ${tabId}`);
      return options.badgeFails === true ? Promise.reject(new Error("no such tab")) : Promise.resolve();
    },
    clearProblem: (tabId) => {
      log.push(`clear ${tabId}`);
      return Promise.resolve();
    },
  };
  const reporter = createEntryReporter({ banner, badge, note: (message) => void notes.push(message) });
  return { reporter, log, banners, notes };
}

describe("репортёр входа", () => {
  it("отказ: плашка на ту же вкладку и «!» на её значке", async () => {
    const { reporter, log, banners } = setupReporter();
    await reporter.report(5, { kind: "outcome", outcome: { kind: "no_video" } });
    assert.deepEqual(banners.map(({ tabId, report }) => [tabId, report.id]), [[5, "no_video"]]);
    assert.deepEqual(log, ["mark 5"]);
  });

  it("удача: плашка есть, вкладочный значок снимается (прошлый отказ не висит)", async () => {
    const { reporter, log, banners } = setupReporter();
    await reporter.report(5, { kind: "outcome", outcome: { kind: "started", player_window: "new" } });
    assert.equal(banners[0]?.report.id, "opened");
    assert.deepEqual(log, ["clear 5"]);
  });

  it("плашка не доставлена (на странице нет скрипта): значок всё равно ставится, ничего не падает", async () => {
    const { reporter, log, notes } = setupReporter({ bannerFails: true });
    await reporter.report(9, { kind: "link_not_opened" });
    assert.deepEqual(log, ["mark 9"]);
    assert.deepEqual(notes, ["entry banner was not delivered"]);
  });

  it("сбой значка не отменяет плашку и не выходит наружу", async () => {
    const { reporter, banners, notes } = setupReporter({ badgeFails: true });
    await reporter.report(9, { kind: "outcome", outcome: { kind: "protected" } });
    assert.equal(banners.length, 1);
    assert.deepEqual(notes, ["entry badge was not updated"]);
  });

  it("forgetProblem: снимает вкладочный «!» без плашки; сбой значка не выходит наружу", async () => {
    const { reporter, log, banners } = setupReporter();
    await reporter.forgetProblem(5);
    assert.deepEqual(log, ["clear 5"]);
    assert.deepEqual(banners, []);
    const broken = setupReporter();
    const failing = createEntryReporter({
      banner: { show: () => Promise.resolve() },
      badge: { markProblem: () => Promise.resolve(), clearProblem: () => Promise.reject(new Error("gone")) },
      note: (message) => void broken.notes.push(message),
    });
    await failing.forgetProblem(1);
    assert.deepEqual(broken.notes, ["entry badge was not cleared"]);
  });

  it("исключение (не отказ промиса) из шва тоже гасится", async () => {
    const notes: string[] = [];
    const reporter = createEntryReporter({
      banner: { show: () => { throw new Error("sync"); } },
      badge: { markProblem: () => { throw new Error("sync"); }, clearProblem: () => Promise.resolve() },
      note: (message) => void notes.push(message),
    });
    await reporter.report(1, { kind: "outcome", outcome: { kind: "failed", reason: "x" } });
    assert.deepEqual(notes, ["entry banner was not delivered", "entry badge was not updated"]);
  });
});
