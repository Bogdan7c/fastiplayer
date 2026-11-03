// Сборка экстеншена: node scripts/build.ts
// Выход: dist/chrome/ и dist/firefox/ — в корне manifest.json и всё, на что он ссылается, ничего лишнего
// (так каталог можно упаковать в .crx / .xpi в сессии 07a). Сборка детерминирована: без меток времени,
// без sourcemap в выпуске (они не ссылаются из манифеста и раздули бы пакет магазина).
import { build as esbuildBuild, type Plugin } from "esbuild";
import { cp, mkdir, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { allCarrierRefusalSets, carrierFileName, encodeRefusedFamilies } from "../src/shared/codec-policy.ts";
import { buildManifest, extensionRoot, loadIdentity, type JsonObject, type TargetBrowser } from "./manifest.ts";

// Минимальные версии браузеров:
//  - Chrome 148: с этой версии ключ манифеста `message_serialization: "structured_clone"` переводит
//    сообщения экстеншена (контент-скрипт ⇄ фон) со JSON на структурное клонирование
//    (developer.chrome.com/blog/structured-clone-messaging). Без него `Uint8Array` с байтами видео
//    приходит в фон объектом {"0":…} — разведка 09a на Chromium 156: 64 МиБ за 5,8 с и мусор вместо
//    байтов, с ключом — целые байты за ~0,4 с; base64 больше не нужен (решения 82/152).
//    Активный WebSocket продлевает жизнь service worker с 116 — 148 это тоже покрывает.
//  - Firefox 140: `world: "MAIN"` в content_scripts и match_origin_as_fallback есть уже с 128, но
//    `data_collection_permissions` (обязателен для новых экстеншенов AMO) распознаётся с 140 (ESR).
const ESBUILD_TARGET: Record<TargetBrowser, string> = { chrome: "chrome148", firefox: "firefox140" };

// Части экстеншена: имя выходного файла -> точка входа. Каждая часть — отдельный IIFE-бандл.
const ENTRY_POINTS: Record<string, string> = {
  background: "src/background/main.ts",
  content: "src/content/main.ts",
  page: "src/page/main.ts",
  popup: "src/popup/main.ts",
  options: "src/options/main.ts",
};

const TARGETS: readonly TargetBrowser[] = ["chrome", "firefox"];

// Отладочная сборка записи потока страницы (сессия 09b2): `npm run build -- --stream-recorder`.
// Код записи подключается подменой двух «швов» (src/page/recorder-seam.ts и
// src/worker/recorder-seam.ts) на отладочные варианты — в обычной сборке эти модули в бандл не
// попадают вовсе, поэтому в dist/* нет ни строки рекордера (проверяет tests/stream-recorder-build.test.ts).
// Отладочная сборка идёт в отдельный каталог, чтобы не затереть продуктовый dist/.
const STREAM_RECORDER_FLAG = "--stream-recorder";
const STREAM_RECORDER_BROWSER_DEFINE = "__FASTIPLAYER_STREAM_RECORDER_BROWSER__";
const STREAM_RECORDER_OUTPUT_DIR = "dist-stream-recorder";
/** Швы: путь продуктового файла -> отладочная замена. */
const STREAM_RECORDER_SEAMS: ReadonlyArray<{ readonly product: RegExp; readonly replacement: string }> = [
  { product: /src[\\/]page[\\/]recorder-seam\.ts$/, replacement: "src/page/stream-recorder/page-seam-enabled.ts" },
  { product: /src[\\/]worker[\\/]recorder-seam\.ts$/, replacement: "src/worker/recorder-seam-enabled.ts" },
];

/** Параметры сборки. По умолчанию — продуктовая, без рекордера. */
export interface BuildOptions {
  readonly streamRecorder?: boolean;
}

/**
 * Настройки esbuild для рекордера: в отладочной сборке плагин подменяет швы, а `define` сообщает
 * целевой браузер; в продуктовой — пусто (швы остаются продуктовыми заглушками).
 */
function streamRecorderBuildSettings(target: TargetBrowser, options: BuildOptions): { define: Record<string, string>; plugins: Plugin[] } {
  if (options.streamRecorder !== true) return { define: {}, plugins: [] };
  const plugin: Plugin = {
    name: "stream-recorder-seams",
    setup(build) {
      // Подмена по пути разрешённого файла: сначала даём esbuild разрешить импорт как обычно.
      build.onResolve({ filter: /recorder-seam\.ts$/ }, async (args) => {
        if (args.pluginData === "stream-recorder-seam") return undefined;
        const resolved = await build.resolve(args.path, { kind: args.kind, resolveDir: args.resolveDir, pluginData: "stream-recorder-seam" });
        const seam = STREAM_RECORDER_SEAMS.find((candidate) => candidate.product.test(resolved.path));
        return seam === undefined ? resolved : { path: join(extensionRoot, seam.replacement) };
      });
    },
  };
  return { define: { [STREAM_RECORDER_BROWSER_DEFINE]: JSON.stringify(target) }, plugins: [plugin] };
}

// Хук выделенного воркера (сессия 09b, решение 63): отдельный IIFE-бандл, который НЕ лежит в dist
// файлом, а вшит строкой в page.js. Обёртка `Worker` MAIN-мира кладёт его в начало blob-скрипта
// воркера (своего файла у экстеншена воркер страницы загрузить не может: адрес chrome-extension://
// для `importScripts` страницы закрыт). Имя ниже — то, что подставляется в src/page/main.ts.
const WORKER_HOOK_ENTRY_POINT = "src/worker/main.ts";
const WORKER_HOOK_SOURCE_DEFINE = "__FASTIPLAYER_WORKER_HOOK_SOURCE__";

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

// Файлы-курьеры политики кодеков (решение 43): их нет в манифесте — фон регистрирует нужный динамически
// (`scripting.registerContentScripts`), поэтому тест сборки сверяет их с `allCarrierRefusalSets()`.
// Файл не принимает параметров, потому набор семейств «запекается» подстановкой при сборке: по файлу
// на каждое непустое подмножество семейств (15), каждый — около килобайта.
async function buildCodecCarriers(target: TargetBrowser, outputDir: string): Promise<void> {
  for (const families of allCarrierRefusalSets()) {
    await esbuildBuild({
      absWorkingDir: extensionRoot,
      entryPoints: ["src/content/codec-carrier-entry.ts"],
      outfile: join(outputDir, carrierFileName(families)),
      define: { __CARRIER_REFUSED_FAMILIES__: JSON.stringify(encodeRefusedFamilies(families)) },
      bundle: true,
      format: "iife",
      target: ESBUILD_TARGET[target],
      minify: false,
      sourcemap: false,
      legalComments: "none",
      logLevel: "warning",
    });
  }
}

/**
 * Текст бандла хука воркера (в памяти, без файла). Детерминирован, как и вся сборка; тесты берут
 * его отсюда же, чтобы проверять ровно то, что поедет в воркер.
 */
export async function buildWorkerHookSource(target: TargetBrowser, options: BuildOptions = {}): Promise<string> {
  const result = await esbuildBuild({
    absWorkingDir: extensionRoot,
    entryPoints: [WORKER_HOOK_ENTRY_POINT],
    ...streamRecorderBuildSettings(target, options),
    write: false,
    bundle: true,
    format: "iife",
    target: ESBUILD_TARGET[target],
    minify: false,
    sourcemap: false,
    legalComments: "none",
    logLevel: "warning",
  });
  const output = result.outputFiles[0];
  if (output === undefined || result.outputFiles.length !== 1) throw new Error("worker hook bundle: expected exactly one output file");
  return output.text;
}

/**
 * Что вшить в page.js как хук воркера для этого браузера.
 *
 * Firefox MSE в воркере не умеет, а обёртка `Worker` там ломает Twitch (решение 63, 00c), поэтому
 * обёртка в Firefox не ставится никогда. Пустая строка вместо ~200 КБ хука избавляет каждый фрейм
 * Firefox от лишнего разбора — это же удлиняло запуск page.js и раскрыло гонку рукопожатия
 * (находка 09b). Пустой текст MAIN-мир читает как «хука нет» (`src/page/main.ts`).
 */
async function workerHookSourceFor(target: TargetBrowser, options: BuildOptions): Promise<string> {
  return target === "chrome" ? await buildWorkerHookSource(target, options) : "";
}

export async function buildTarget(target: TargetBrowser, outputRoot: string, options: BuildOptions = {}): Promise<string> {
  const outputDir = join(outputRoot, target);
  await rm(outputDir, { recursive: true, force: true });
  await mkdir(outputDir, { recursive: true });

  const workerHookSource = await workerHookSourceFor(target, options);
  const recorderSettings = streamRecorderBuildSettings(target, options);
  await esbuildBuild({
    absWorkingDir: extensionRoot,
    entryPoints: Object.fromEntries(Object.entries(ENTRY_POINTS).map(([name, path]) => [name, path])),
    outdir: outputDir,
    // Текст хука воркера — строковым литералом в page.js (в остальных частях имени нет).
    define: { [WORKER_HOOK_SOURCE_DEFINE]: JSON.stringify(workerHookSource), ...recorderSettings.define },
    plugins: recorderSettings.plugins,
    bundle: true,
    format: "iife",
    target: ESBUILD_TARGET[target],
    minify: false, // читаемый код нужен ревьюерам магазинов
    sourcemap: false,
    legalComments: "none",
    logLevel: "warning",
  });

  await buildCodecCarriers(target, outputDir);

  await cp(join(extensionRoot, "static"), outputDir, { recursive: true });

  const packageVersion = (await readJson("package.json"))["version"] as string;
  const manifest = buildManifest(target, await readJson("manifest/base.json"), await readJson(`manifest/${target}.json`), loadIdentity(), packageVersion);
  await writeFile(join(outputDir, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
  return outputDir;
}

async function main(): Promise<void> {
  const streamRecorder = process.argv.includes(STREAM_RECORDER_FLAG);
  const outputRoot = join(extensionRoot, streamRecorder ? STREAM_RECORDER_OUTPUT_DIR : "dist");
  for (const target of TARGETS) {
    const outputDir = await buildTarget(target, outputRoot, { streamRecorder });
    console.log(`${target}: ${outputDir} (${await directorySize(outputDir)} bytes)`);
  }
}

// Запуск как скрипт; при импорте из тестов main не выполняется.
if (process.argv[1] !== undefined && import.meta.filename === process.argv[1]) {
  await main();
}
