// Отсрочка `URL.revokeObjectURL` (решение 192) сама по себе: учёт удержаний, срок, сбой оригинала,
// прозрачность для всех прочих адресов и то, что она не трогает чужое состояние.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { BlobRevokeDeferral, SITE_SCRIPT_LOAD_DEADLINE_MS } from "../../src/page/worker-capture/blob-revoke-deferral.ts";

const HELD_URL = "blob:https://site.example/1";
const OTHER_URL = "blob:https://site.example/2";

/** «URL» с настоящим revoke-методом (записывает вызовы) и ручными сроками. */
function world(options: { readonly revokeThrows?: boolean } = {}) {
  const revoked: { readonly url: unknown; readonly receiver: unknown }[] = [];
  const createObjectURL = (): string => HELD_URL;
  const owner = {
    createObjectURL,
    revokeObjectURL(this: unknown, url: unknown): void {
      if (options.revokeThrows === true) throw new Error("revoke failed");
      revoked.push({ url, receiver: this });
    },
  };
  const deadlines: { readonly callback: () => void; readonly delayMs: number; cancelled: boolean }[] = [];
  const oddities: string[] = [];
  const deferral = new BlobRevokeDeferral({
    canonicalBlobUrl: (input) => (input.toLowerCase().startsWith("blob:") ? `blob:${input.slice(5)}` : null),
    scheduleTimeout: (callback, delayMs) => {
      const entry = { callback, delayMs, cancelled: false };
      deadlines.push(entry);
      return () => {
        entry.cancelled = true;
      };
    },
    reportOddity: (what) => oddities.push(what),
  });
  const pageRevoke = (url: unknown): unknown => (owner.revokeObjectURL as (url: unknown) => unknown)(url);
  return { owner, revoked, deadlines, oddities, deferral, pageRevoke, createObjectURL };
}

describe("отсрочка revoke: учёт", () => {
  it("удерживаемый адрес: revoke страницы ждёт release, затем исполняется оригиналом ровно один раз", () => {
    const { deferral, owner, revoked, pageRevoke } = world();
    assert.equal(deferral.install(owner), true);
    const hold = deferral.hold(HELD_URL);
    assert.equal(pageRevoke(HELD_URL), undefined);
    assert.deepEqual(revoked, []);
    hold.release();
    assert.deepEqual(revoked, [{ url: HELD_URL, receiver: owner }]);
    hold.release();
    assert.equal(revoked.length, 1, "release идемпотентен");
    assert.equal(deferral.heldUrlCount, 0);
  });

  it("release без revoke страницы — оригинал не зовётся (адрес страница ещё держит)", () => {
    const { deferral, owner, revoked } = world();
    deferral.install(owner);
    deferral.hold(HELD_URL).release();
    assert.deepEqual(revoked, []);
  });

  it("два воркера из одного адреса: revoke — когда отпустит последний", () => {
    const { deferral, owner, revoked, pageRevoke } = world();
    deferral.install(owner);
    const first = deferral.hold(HELD_URL);
    const second = deferral.hold(HELD_URL);
    pageRevoke(HELD_URL);
    first.release();
    assert.deepEqual(revoked, []);
    second.release();
    assert.equal(revoked.length, 1);
  });

  it("срок: release сам, отменённый срок после release не стреляет повторно", () => {
    const { deferral, owner, revoked, deadlines, pageRevoke } = world();
    deferral.install(owner);
    deferral.hold(HELD_URL);
    pageRevoke(HELD_URL);
    assert.equal(deadlines.length, 1);
    assert.equal(deadlines[0]?.delayMs, SITE_SCRIPT_LOAD_DEADLINE_MS);
    deadlines[0]?.callback();
    assert.equal(revoked.length, 1);
    const later = deferral.hold(HELD_URL);
    later.release();
    assert.equal(deadlines[1]?.cancelled, true, "release снимает свой срок");
  });

  it("разное написание одного адреса (канон) — тот же учёт", () => {
    const { deferral, owner, revoked, pageRevoke } = world();
    deferral.install(owner);
    const hold = deferral.hold(HELD_URL);
    pageRevoke(`BLOB:${HELD_URL.slice(5)}`);
    assert.equal(revoked.length, 0);
    hold.release();
    assert.equal(revoked[0]?.url, HELD_URL);
  });
});

describe("отсрочка revoke: прозрачность и сбои", () => {
  it("неудерживаемые адреса, не-строки и пустой вызов — сразу оригиналу, с тем же this и аргументами", () => {
    const { deferral, owner, revoked, pageRevoke } = world();
    deferral.install(owner);
    deferral.hold(HELD_URL);
    pageRevoke(OTHER_URL);
    const objectArgument = { toString: () => HELD_URL };
    pageRevoke(objectArgument);
    (owner.revokeObjectURL as () => unknown)();
    assert.deepEqual(
      revoked.map((call) => call.url),
      [OTHER_URL, objectArgument, undefined],
    );
    assert.ok(revoked.every((call) => call.receiver === owner));
  });

  it("сбой оригинала при отложенном revoke — одна строка отладки, без исключения наружу", () => {
    const { deferral, owner, oddities, pageRevoke } = world({ revokeThrows: true });
    deferral.install(owner);
    const hold = deferral.hold(HELD_URL);
    pageRevoke(HELD_URL);
    assert.doesNotThrow(() => hold.release());
    assert.deepEqual(oddities, ["deferred blob revoke failed"]);
  });

  it("исключение оригинала на неудерживаемом адресе уходит странице как было", () => {
    const { deferral, owner, pageRevoke } = world({ revokeThrows: true });
    deferral.install(owner);
    assert.throws(() => pageRevoke(OTHER_URL), /revoke failed/);
  });

  it("метода нет или он не перезаписывается — install false, ничего не тронуто", () => {
    const empty = {};
    assert.equal(world().deferral.install(empty), false);
    assert.deepEqual(Object.getOwnPropertyNames(empty), []);
    const frozenRevoke = (): void => {};
    const frozen = {};
    Object.defineProperty(frozen, "revokeObjectURL", { value: frozenRevoke, writable: false, configurable: false });
    assert.equal(world().deferral.install(frozen), false);
    assert.equal((frozen as { revokeObjectURL: unknown }).revokeObjectURL, frozenRevoke);
  });

  it("не трогает чужое: createObjectURL и флаги revokeObjectURL — как были", () => {
    const { deferral, owner, createObjectURL } = world();
    const before = Object.getOwnPropertyDescriptor(owner, "revokeObjectURL");
    deferral.install(owner);
    const after = Object.getOwnPropertyDescriptor(owner, "revokeObjectURL");
    assert.equal(owner.createObjectURL, createObjectURL);
    assert.deepEqual({ ...after, value: undefined }, { ...before, value: undefined });
    assert.equal((after?.value as Function).name, "revokeObjectURL");
  });
});
