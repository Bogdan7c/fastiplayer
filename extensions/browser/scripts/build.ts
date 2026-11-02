// Сборка экстеншена: node scripts/build.ts
// Выход: dist/chrome/ и dist/firefox/ — в корне manifest.json и всё, на что он ссылается, ничего лишнего
// (так каталог можно упаковать в .crx / .xpi в сессии 07a). Сборка детерминирована: без меток времени,
// без sourcemap в выпуске (они не ссылаются из манифеста и раздули бы пакет магазина).
import { build as esbuildBuild } from "esbuild";
import { cp, mkdir, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { buildManifest, extensionRoot, loadIdentity, type JsonObject, type TargetBrowser } from "./manifest.ts";

// Минимальные версии браузеров:
//  - Chrome 116: с этой версии активный WebSocket продлевает жизнь service worker (иначе соединение
//    с плеером обрывалось бы через 30 с простоя).
//  - Firefox 140: `world: "MAIN"` в content_scripts и match_origin_as_fallback есть уже с 128, но
//    `data_collection_permissions` (обязателен для новых экстеншенов AMO) распознаётся с 140 (ESR).
const ESBUILD_TARGET: Record<TargetBrowser, string> = { chrome: "chrome116", firefox: "firefox140" };

// Части экстеншена: имя выходного файла -> точка входа. Каждая часть — отдельный IIFE-бандл.
const ENTRY_POINTS: Record<string, string> = {
  background: "src/background/main.ts",
  content: "src/content/main.ts",
  page: "src/page/main.ts",
  popup: "src/popup/main.ts",
  options: "src/options/main.ts",
};

const TARGETS: readonly TargetBrowser[] = ["chrome", "firefox"];

async function readJson(relativePath: string): Promise<JsonObject> {
  return JSON.parse(await readFile(join(extensionRoot, relativePath), "utf8")) as JsonObject;
}

async function directorySize(directory: string): Promise<number> {
  let total = 0;
  for (const entry of await readdir(directory, { withFileTypes: true, recursive: true })) {
    if (entry.isFile()) total += (await stat(join(entry.parentPath, entry.name))).size;
  }
  return total;
}

export async function buildTarget(target: TargetBrowser, outputRoot: string): Promise<string> {
  const outputDir = join(outputRoot, target);
  await rm(outputDir, { recursive: true, force: true });
  await mkdir(outputDir, { recursive: true });

  await esbuildBuild({
    absWorkingDir: extensionRoot,
    entryPoints: Object.fromEntries(Object.entries(ENTRY_POINTS).map(([name, path]) => [name, path])),
    outdir: outputDir,
    bundle: true,
    format: "iife",
    target: ESBUILD_TARGET[target],
    minify: false, // читаемый код нужен ревьюерам магазинов
    sourcemap: false,
    legalComments: "none",
    logLevel: "warning",
  });

  await cp(join(extensionRoot, "static"), outputDir, { recursive: true });

  const packageVersion = (await readJson("package.json"))["version"] as string;
  const manifest = buildManifest(target, await readJson("manifest/base.json"), await readJson(`manifest/${target}.json`), loadIdentity(), packageVersion);
  await writeFile(join(outputDir, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
  return outputDir;
}

async function main(): Promise<void> {
  const outputRoot = join(extensionRoot, "dist");
  for (const target of TARGETS) {
    const outputDir = await buildTarget(target, outputRoot);
    console.log(`${target}: ${outputDir} (${await directorySize(outputDir)} bytes)`);
  }
}

// Запуск как скрипт; при импорте из тестов main не выполняется.
if (process.argv[1] !== undefined && import.meta.filename === process.argv[1]) {
  await main();
}
