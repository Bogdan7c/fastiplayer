# Web-media operational errors

Web-media failures are typed by stage and ownership. The UI may render a shorter
message, but it must not collapse an excluded profile, temporary starvation,
authentication failure, cancellation and fatal post-barrier failure into one
generic “не открылось”.

## Что означает сообщение

| Категория | Что произошло | Что остаётся неизменным | Действие пользователя |
| --- | --- | --- | --- |
| Invalid URL | Locator синтаксически некорректен | Queue и current playback | Исправить URL; supported top-level schemes: HTTP, HTTPS, FTP, FTPS |
| Unsupported scheme | Scheme не входит в approved input vocabulary | Queue и current playback | Не повторять RTSP/RTP/MMS/RTMPS/RTMPT/RTMPTE/`file:` как web URL; local file открывается отдельным action |
| `ProfileExcludedInputScheme` / `ProfileExcluded` | Identity известна, но намеренно не поддерживается | Queue и current playback до barrier | Для RTMP/RTMPE и media с DRM/private-live dependency не ожидать provider installation или fallback |
| No approved row | Для provider shape нет утверждённой profile row | Queue и current playback до barrier | Считать feature отсутствующей; нужен отдельный profile extension, а не другой URL |
| Unknown/incompatible contract | `yt-dlp` вернул неизвестную или несериализуемую shape | Queue и current playback до barrier | Обновить report без secret material; не пытаться скрыть identity generic fallback-ом |
| Provider unavailable | Row реализована, но exact provider capability не зарегистрирована/не собрана | Queue и current playback до barrier | Проверить build/features; это installation problem, не unsupported site |
| Authentication missing/rejected/expired | Credentials не были доступны, отклонены либо протухли | Secret scope не расширяется; до barrier current playback сохраняется | Исправить user-owned system `yt-dlp` config/cookie source и повторить open |
| Secret scope rejected | Redirect/target вышел за разрешённый origin/path scope | Authorization/Cookie не отправляются новому origin | Использовать корректный source; не копировать credential в report |
| Network/transport error | DNS, connection, TLS, HTTP/FTP status или server behavior не позволили open/read | До barrier current playback сохраняется | Проверить сеть и explicit URL; для FTP(S) также доступность REST/seek semantics |
| `TemporarilyUnavailable` | Live/DVR пока не имеет следующего packet/segment | Это не EOF и не fatal error | Подождать retry; отсутствие busy-spin и отзывчивость UI обязательны |
| Seek target expired | DVR window уже вытеснила requested time | Current live source остаётся установленным | Выбрать доступную позицию или live edge |
| Resource/authorization expired | Live endpoint либо auth material требуют refresh | Старое поколение не становится новым active source | Разрешить bounded re-extraction/refresh; при повторном отказе обновить user auth |
| Cancelled/superseded/stale | Новый request, shutdown или generation fencing отменили работу | Stale completion не публикуется и не меняет current | Обычно действий не нужно; повторить только если user intent всё ещё актуален |
| Pre-barrier open/import/switch failure | Ошибка произошла до exact Installed receipt | Queue/current playback и Playing/Paused intent сохраняются | Исправить input/selection и повторить |
| Post-barrier terminal failure | Authorization уже принята и lifecycle перешёл commit boundary | Rollback старого playback больше не обещан | Считать ошибку terminal, открыть media заново; не называть её recoverable rollback |
| Shutdown timeout/failure | Worker не завершился в bounded deadline | Handle не забывается; late reap остаётся owner responsibility | Сохранить secret-safe diagnostics и завершить приложение штатно |

## Live/DVR contract

Neutral demux contract возвращает `TemporarilyUnavailable(retry_hint)` для
starvation. Он не выдаёт fake EOF, не превращает ожидание в permanent error и не
блокирует player-owner polling loop. Terminal end публикуется только когда
источник действительно завершён. Seek раньше текущего DVR range является typed
expiry, а не silent clamp, если exact restore невозможен.

Refresh и retry привязаны к source generation и cancellation token. Completion
старого поколения не имеет права заменить active target, headers, cookies,
timeline или sidebar state нового поколения.

## Authentication и trusted system state

Production extraction намеренно сохраняет обычный lookup system/user `yt-dlp`
config, plugins и cookie options. Это manual opt-in trust boundary:

- Fastiplayer не просит и не сохраняет app-owned cookie/browser credential;
- exact acknowledged locator хранится отдельно от transient target, headers,
  cookies и extractor payload;
- Fastiplayer-owned argv не добавляет download/write/exec/postprocessor или
  `--mark-watched` options;
- user config может добавить такие options, plugin является исполняемым Python
  code, а user-owned cookie jar может быть изменён самим `yt-dlp`; эти side
  effects находятся вне app guarantee.

Manual S42 acceptance требует exact system `yt-dlp 2026.07.04`. Другая версия
не считается “почти совместимой”: runner завершает preflight ошибкой до создания
report и runtime launch.

## Что допустимо сохранять в diagnostics

Допустимы safe case ID, typed error category, source generation, provider ID,
container/codec identities в установленных bounds, exit status, profile ID,
workspace HEAD и только его `clean`/`dirty` classification, binary origin,
Fastiplayer executable SHA-256, `yt-dlp` version и его executable SHA-256.
Для explicit external `--binary` workspace HEAD не является source provenance;
report обязан говорить это явно.

Нельзя сохранять raw URL или fixture path, userinfo, query/fragment, resolved
transport target, Authorization/Cookie/Set-Cookie, arbitrary headers, request
body, extractor payload, token, signature или password. FTP/FTPS credentials
редактируются по тому же правилу, что HTTP(S).

Generated manual report является заготовкой для человека. Не вставляйте в его
checkbox notes raw URL, cookies или captured extractor JSON. Если для разбора
нужен raw capture, храните его отдельно как user-owned sensitive artifact и не
прикладывайте к публичному issue.

## Где проверить границу

- Exact runtime rows и exclusions:
  [web-media-compatibility-matrix.md](web-media-compatibility-matrix.md).
- Safe manual runner и checklist:
  [web-media-s42-final-acceptance.md](web-media-s42-final-acceptance.md).
- Historical progressive ownership/evidence:
  [progressive-web-s27.md](progressive-web-s27.md).
