// Реестры адаптеров (14b, переезд без смены поведения): порядок 09d и 13b2 сохранён, паспорта честные,
// каждый мир собирает только свои способности.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createAllAdOrgans, ISOLATED_WORLD_SITES } from "../../src/adapters/registry/isolated-world.ts";
import { PAGE_WORLD_SITES, SUBTITLE_NAMING_ADAPTERS } from "../../src/adapters/registry/page-world.ts";
import { describeSiteAdapter, type SiteAdapterIdentity } from "../../src/adapters/site-identity.ts";
import { createFakeAdView, FakeNode } from "./fake-ad-dom.ts";

/** Паспорт годится для журнала: имя без пробелов, версия — целое ≥ 1. */
function assertHonestIdentity(identity: SiteAdapterIdentity): void {
  assert.match(identity.id, /^[a-z0-9-]+$/u);
  assert.ok(Number.isInteger(identity.version) && identity.version >= 1, `${identity.id}: версия ${identity.version}`);
}

describe("реестр изолированного мира", () => {
  it("читатели рекламы идут в порядке 09d: YouTube, IMA, PlayerJS", () => {
    const view = createFakeAdView(new FakeNode("body"), { hostname: "example.org" });
    assert.deepEqual(createAllAdOrgans(view).map((organs) => organs.family), ["youtube", "ima", "playerjs"]);
  });

  it("у каждого сайта свой честный паспорт, имена не повторяются", () => {
    const ids = ISOLATED_WORLD_SITES.map((site) => site.identity.id);
    assert.equal(new Set(ids).size, ids.length);
    for (const site of ISOLATED_WORLD_SITES) assertHonestIdentity(site.identity);
  });
});

describe("реестр мира страницы", () => {
  it("общий читатель адреса — последним, адаптеры сайтов — раньше него (13b2)", () => {
    assert.deepEqual(SUBTITLE_NAMING_ADAPTERS.map((adapter) => adapter.name), ["youtube-timedtext", "generic-address"]);
  });

  it("паспорта мира страницы честные", () => {
    for (const site of PAGE_WORLD_SITES) assertHonestIdentity(site.identity);
  });
});

describe("строка паспорта", () => {
  it("имя@версия, без лишнего", () => {
    assert.equal(describeSiteAdapter({ id: "youtube", version: 3 }), "youtube@3");
  });
});
