// Общее для тестов протокола: путь к общим фикстурам и их загрузка.
// Путь считается от расположения этого файла, а не от текущей папки запуска.

import { readFileSync } from "node:fs";
import { join } from "node:path";

/** Папка общих фикстур Rust и TypeScript. */
export const FIXTURES_ROOT = join(
  import.meta.dirname,
  "..",
  "..",
  "..",
  "..",
  "crates",
  "browser-bridge-protocol",
  "fixtures",
);

export type FixtureSchema = "extension_control" | "player_control" | "page_session" | "player_session" | "data_frame";

export type FixtureExpectation =
  | { readonly result: "ok"; readonly frame?: Readonly<Record<string, unknown>> }
  | { readonly result: "error"; readonly kind: string };

export interface FixtureCase {
  readonly file: string;
  readonly schema: FixtureSchema;
  readonly expect: FixtureExpectation;
}

export interface FixtureManifest {
  readonly cases: readonly FixtureCase[];
  readonly streams: readonly string[];
}

export function loadManifest(): FixtureManifest {
  return JSON.parse(readFileSync(join(FIXTURES_ROOT, "manifest.json"), "utf8")) as FixtureManifest;
}

/** Байты фикстуры по пути относительно папки фикстур. */
export function readFixtureBytes(relativePath: string): Uint8Array {
  return new Uint8Array(readFileSync(join(FIXTURES_ROOT, relativePath)));
}

/** Склейка байтов. */
export function concatBytes(parts: readonly Uint8Array[]): Uint8Array {
  const total = parts.reduce((sum, part) => sum + part.length, 0);
  const joined = new Uint8Array(total);
  let position = 0;
  for (const part of parts) {
    joined.set(part, position);
    position += part.length;
  }
  return joined;
}
