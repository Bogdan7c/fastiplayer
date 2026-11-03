// Скачивание записи файлом из документа страницы: `<a download>` с blob-адресом.
//
// Почему так, а не `downloads` API из фона (решение пакета П2):
//  - не нужно отладочное разрешение `downloads` в манифесте: манифест отладочной сборки совпадает с
//    продуктовым, и рекордер нельзя «включить» одним лишь манифестом;
//  - файл в сотни МБ не надо гнать через сообщения «MAIN → изолированный мир → фон» (у них потолок
//    размера события и лишние копии): Blob собирается там, где лежит запись;
//  - срабатывает из отладочного протокола без окон и запросов: Firefox 158 сохраняет `blob:` по
//    `download` без диалога (`browser.download.useDownloadDir` = true по умолчанию), Chromium — первое
//    скачивание страницы без запроса; следующие автоскачивания Chromium спрашивает, если нет жеста
//    пользователя, — поэтому CDP-вызов делается с `userGesture: true` либо каталог задаётся
//    `Browser.setDownloadBehavior` (стенд П3).
// Все нужные функции DOM снимаются при установке, до скриптов страницы.

/** Скачивание файла из частей; возвращает размер в байтах. */
export interface FileDownloader {
  download(parts: readonly Uint8Array[], filename: string): number;
}

/** Окно с тем, что нужно скачиванию. */
export interface DownloadScope {
  readonly Blob: new (parts: BlobPart[], options?: BlobPropertyBag) => Blob;
  readonly URL: { createObjectURL(blob: Blob): string; revokeObjectURL(url: string): void };
  readonly document: Document;
  readonly setTimeout: (callback: () => void, delayMs: number) => unknown;
  readonly Document: { readonly prototype: object };
  readonly HTMLElement: { readonly prototype: object };
  readonly Node: { readonly prototype: object };
}

/** Через сколько отпустить blob-адрес: браузер уже начал скачивание, но большой файл берёт паузу. */
const OBJECT_URL_LIFETIME_MS = 120_000;

function capturedFunction(owner: object, name: string): (target: unknown, args: unknown[]) => unknown {
  const original: unknown = Object.getOwnPropertyDescriptor(owner, name)?.value;
  if (typeof original !== "function") throw new Error(`download: ${name} is missing`);
  return (target, args) => Reflect.apply(original, target, args);
}

/** Снять функции DOM и вернуть скачивание через якорь. Бросает, если чего-то нет в окне. */
export function createAnchorDownloader(scope: DownloadScope): FileDownloader {
  const BlobConstructor = scope.Blob;
  const createObjectUrl = scope.URL.createObjectURL.bind(scope.URL);
  const revokeObjectUrl = scope.URL.revokeObjectURL.bind(scope.URL);
  const setTimer = scope.setTimeout.bind(scope);
  const createElement = capturedFunction(scope.Document.prototype, "createElement");
  const click = capturedFunction(scope.HTMLElement.prototype, "click");
  const appendChild = capturedFunction(scope.Node.prototype, "appendChild");
  const removeChild = capturedFunction(scope.Node.prototype, "removeChild");
  const { document } = scope;
  return {
    download(parts, filename) {
      const blob = new BlobConstructor([...parts] as BlobPart[], { type: "application/octet-stream" });
      const url = createObjectUrl(blob);
      const anchor = createElement(document, ["a"]) as HTMLAnchorElement;
      anchor.href = url;
      anchor.download = filename;
      // Firefox надёжнее скачивает якорь, который состоит в документе.
      const host = document.documentElement ?? document.body;
      appendChild(host, [anchor]);
      try {
        click(anchor, []);
      } finally {
        removeChild(host, [anchor]);
      }
      setTimer(() => revokeObjectUrl(url), OBJECT_URL_LIFETIME_MS);
      return blob.size;
    },
  };
}
