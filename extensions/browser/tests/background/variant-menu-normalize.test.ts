// Описание страницы → меню протокола (14b, инвариант 1): что бы ни принёс адаптер, сообщение проходит строгий
// кодек протокола (иначе сеанс порвался бы на `page_message_rejected`).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { normalizeVariantMenus } from "../../src/background/handoff/variant-menu-normalize.ts";
import { encodePageSession, MAX_VARIANT_LABEL_BYTES, MAX_VARIANT_OPTIONS, utf8ByteLength } from "../../src/protocol/index.ts";
import type { PageVariantDescription, PageVariantOption } from "../../src/shared/variant-watch-contract.ts";

const describeWith = (audio: readonly PageVariantOption[], nextEpisode = false): PageVariantDescription => ({
  seriesKey: null,
  episodeKey: null,
  audio,
  quality: [],
  subtitles: [],
  nextEpisode,
});

/** Меню проходит кодек протокола целиком. */
function assertWireSafe(description: PageVariantDescription | null): void {
  const menus = normalizeVariantMenus(description);
  for (const menu of [menus.audio, menus.quality]) {
    const encoded = encodePageSession({ type: "variant_menu_changed", menu });
    assert.ok(encoded.ok, `меню ${menu.kind} не прошло протокол: ${JSON.stringify(encoded)}`);
  }
}

describe("нормализация меню вариантов", () => {
  it("пометка «Auto» (14b2) остаётся в фоне и не уходит плееру", () => {
    const menus = normalizeVariantMenus({
      ...describeWith([]),
      quality: [
        { id: "auto", label: "Auto", selected: true, automatic: true },
        { id: "q720", label: "720p", selected: false },
      ],
    });
    assert.deepEqual([...menus.automatic.quality], ["auto"]);
    assert.deepEqual(menus.quality.options[0], { id: "auto", label: "Auto", selection: "selected" });
    assertWireSafe({ ...describeWith([]), quality: [{ id: "auto", label: "Auto", selected: true, automatic: true }] });
  });

  it("годное меню — как есть, «выбран» переходит в selected", () => {
    const menus = normalizeVariantMenus(describeWith([{ id: "dub-a", label: "Озвучка «Студия»", selected: true }], true));
    assert.deepEqual(menus.audio, { kind: "audio", options: [{ id: "dub-a", label: "Озвучка «Студия»", selection: "selected" }] });
    assert.equal(menus.nextEpisode, "available");
    assertWireSafe(describeWith([{ id: "dub-a", label: "Озвучка «Студия»", selected: true }]));
  });

  it("негодный id (пробел, кириллица, пустой, длиннее 64) и повтор id — пункт выброшен", () => {
    const menus = normalizeVariantMenus(
      describeWith([
        { id: "with space", label: "A", selected: false },
        { id: "дубляж", label: "B", selected: false },
        { id: "x".repeat(65), label: "C", selected: false },
        { id: "ok", label: "D", selected: false },
        { id: "ok", label: "E", selected: true },
      ]),
    );
    assert.deepEqual(menus.audio.options, [{ id: "ok", label: "D", selection: "available" }]);
  });

  it("подпись: разрывы — пробел, управляющие и bidi убраны, края обрезаны, длинная урезана по символу; пустая — пункт выброшен", () => {
    const longLabel = "Ї".repeat(MAX_VARIANT_LABEL_BYTES);
    const menus = normalizeVariantMenus(
      describeWith([
        { id: "a", label: "  Dub\n\tA\u202e\u0007 ", selected: false },
        { id: "b", label: longLabel, selected: false },
        { id: "c", label: " \u202e\u0000 ", selected: false },
      ]),
    );
    assert.equal(menus.audio.options[0]?.label, "Dub A");
    assert.ok(utf8ByteLength(menus.audio.options[1]?.label ?? "") <= MAX_VARIANT_LABEL_BYTES);
    assert.equal(menus.audio.options.length, 2);
  });

  it("выбранных несколько — selected только у первого; пунктов больше предела — хвост отрезан", () => {
    const many = Array.from({ length: MAX_VARIANT_OPTIONS + 10 }, (_, index) => ({ id: `v${index}`, label: `V${index}`, selected: true }));
    const menus = normalizeVariantMenus(describeWith(many));
    assert.equal(menus.audio.options.length, MAX_VARIANT_OPTIONS);
    assert.equal(menus.audio.options.filter((option) => option.selection === "selected").length, 1);
    assert.equal(menus.audio.options[0]?.selection, "selected");
    assertWireSafe(describeWith(many));
  });

  it("описания нет — пустые меню и «следующей серии нет», тоже годные для протокола", () => {
    const menus = normalizeVariantMenus(null);
    assert.deepEqual([menus.audio.options, menus.quality.options, menus.nextEpisode], [[], [], "unavailable"]);
    assertWireSafe(null);
  });
});
