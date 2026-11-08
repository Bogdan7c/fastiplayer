// Стоп-лист и политика сайта режима 2 (12a): сопоставление адреса, порядок правил.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { DEFAULT_AUTO_HANDOFF_SETTINGS, normalizeStopListEntry, type AutoHandoffSettings } from "../../src/shared/auto-handoff-settings.ts";
import { sitePolicyOf, siteRuleFor, type SitePolicy } from "../../src/background/auto-handoff/site-policy.ts";
import { BUILT_IN_STOP_LIST_GROUPS, addressIsInStopList, builtInStopListEntries, parseWebAddress } from "../../src/background/auto-handoff/stop-list.ts";

const inList = (url: string, entries: readonly string[]): boolean => {
  const address = parseWebAddress(url);
  assert.ok(address !== undefined, url);
  return addressIsInStopList(address, entries);
};

describe("сопоставление адреса со стоп-листом", () => {
  it("хост записи и его поддомены совпадают, www. и порт не мешают, чужой хост с тем же окончанием — нет", () => {
    assert.equal(inList("https://zoom.us/j/1", ["zoom.us"]), true);
    assert.equal(inList("https://us02web.zoom.us/j/1", ["zoom.us"]), true);
    assert.equal(inList("https://www.zoom.us:8443/", ["zoom.us"]), true);
    assert.equal(inList("https://notzoom.us/", ["zoom.us"]), false);
    assert.equal(inList("https://zoom.us.evil.example/", ["zoom.us"]), false);
  });

  it("путь совпадает на границе сегмента", () => {
    const entries = ["youtube.com/shorts"];
    assert.equal(inList("https://www.youtube.com/shorts", entries), true);
    assert.equal(inList("https://www.youtube.com/shorts/", entries), true);
    assert.equal(inList("https://m.youtube.com/shorts/abc123", entries), true);
    assert.equal(inList("https://www.youtube.com/shortsx", entries), false);
    assert.equal(inList("https://www.youtube.com/watch?v=1", entries), false);
  });

  it("не http(s) не разбирается вовсе; мусорная запись не совпадает ни с чем", () => {
    for (const url of ["file:///home/a.mp4", "chrome://extensions", "about:blank", "ftp://zoom.us/", "not a url", ""]) assert.equal(parseWebAddress(url), undefined, url);
    assert.equal(inList("https://zoom.us/", ["a b", ""]), false);
  });

  it("встроенный список: все записи в нормальном виде, группы названы, главные сайты внутри", () => {
    for (const entry of builtInStopListEntries()) assert.equal(normalizeStopListEntry(entry), entry, entry);
    assert.deepEqual(BUILT_IN_STOP_LIST_GROUPS.map((group) => group.id), ["calls", "social_feeds", "banks_and_payments"]);
    for (const url of ["https://meet.google.com/abc", "https://www.reddit.com/r/x", "https://www.paypal.com/", "https://youtube.com/shorts/1"]) assert.equal(inList(url, builtInStopListEntries()), true, url);
    for (const url of ["https://www.youtube.com/watch?v=1", "https://example.org/", "https://google.com/"]) assert.equal(inList(url, builtInStopListEntries()), false, url);
  });
});

const ON: AutoHandoffSettings = { ...DEFAULT_AUTO_HANDOFF_SETTINGS, enabled: true };
const HAND_OFF: SitePolicy = { kind: "hand_off" };
const skip = (reason: SitePolicy extends infer P ? (P extends { reason: infer R } ? R : never) : never): SitePolicy => ({ kind: "skip", reason });

describe("политика сайта", () => {
  it("правило сайта ищется от точного хоста вверх по родителям, но не до голого TLD", () => {
    assert.equal(siteRuleFor("video.example.com", { "example.com": "never" }), "never");
    assert.equal(siteRuleFor("a.b.example.com", { "b.example.com": "always", "example.com": "never" }), "always");
    assert.equal(siteRuleFor("example.com", { com: "never" }), undefined);
    assert.equal(siteRuleFor("localhost", { localhost: "ask" }), "ask");
    assert.equal(siteRuleFor("toString.example.com", {}), undefined);
  });

  it("таблица порядка: каждая строка — адрес, настройка, ожидаемое решение", () => {
    const table: readonly { name: string; url: string; settings: AutoHandoffSettings; expected: SitePolicy }[] = [
      { name: "не веб-страница", url: "about:blank", settings: { ...ON, site_rules: { "about.blank": "always" } }, expected: skip("not_web_page") },
      { name: "свой стоп-лист сильнее своего правила always", url: "https://news.example/a", settings: { ...ON, own_stop_list: ["news.example"], site_rules: { "news.example": "always" } }, expected: skip("own_stop_list") },
      { name: "свой стоп-лист с путём трогает только раздел", url: "https://news.example/live", settings: { ...ON, own_stop_list: ["news.example/video"] }, expected: HAND_OFF },
      { name: "своё never", url: "https://a.example/", settings: { ...ON, site_rules: { "a.example": "never" } }, expected: skip("site_never") },
      { name: "своё ask до 12b — не сейчас", url: "https://a.example/", settings: { ...ON, site_rules: { "a.example": "ask" } }, expected: skip("site_ask_pending_12b") },
      { name: "правило родителя действует на поддомен", url: "https://video.a.example/", settings: { ...ON, site_rules: { "a.example": "never" } }, expected: skip("site_never") },
      { name: "своё always сильнее встроенного стоп-листа", url: "https://www.reddit.com/r/video", settings: { ...ON, site_rules: { "reddit.com": "always" } }, expected: HAND_OFF },
      { name: "своё always работает и при выключенном режиме", url: "https://a.example/", settings: { ...DEFAULT_AUTO_HANDOFF_SETTINGS, site_rules: { "a.example": "always" } }, expected: HAND_OFF },
      { name: "встроенный стоп-лист при включённом режиме", url: "https://meet.google.com/x", settings: ON, expected: skip("built_in_stop_list") },
      { name: "включён, правила нет, unknown always", url: "https://a.example/", settings: ON, expected: HAND_OFF },
      { name: "включён, правила нет, unknown ask", url: "https://a.example/", settings: { ...ON, unknown_site_rule: "ask" }, expected: skip("site_ask_pending_12b") },
      { name: "режим выключен, правила нет", url: "https://a.example/", settings: DEFAULT_AUTO_HANDOFF_SETTINGS, expected: skip("mode_off") },
    ];
    for (const row of table) assert.deepEqual(sitePolicyOf(row.url, row.settings), row.expected, row.name);
  });
});
