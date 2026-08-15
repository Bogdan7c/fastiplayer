# S37 progressive FTP/FTPS transport (2026-07-25)

Связанные знания: `mem:media-services/core`, `mem:media-services/web-transport-s21t-2026-07-21`, `mem:media-services/secret-safe-locators-s10b`.

## Реализованный boundary

- `source-core` владеет FTP policy, exact secret target, connect/login, passive data-channel policy, REST/SIZE probing и lifecycle transfer-а.
- `web-media-ftp` — concrete S22 provider для `ftp://` и explicit-TLS `ftps://`.
- `service-ytdlp` строит neutral requests через `YtDlpProgressiveTransportRequestContext`, который содержит exact HTTP и FTP provider contexts. Для `StreamLayout::Separate` каждый physical component маршрутизируется по собственной `TransportFamily`, поэтому mixed HTTP+FTP поддержан без all-or-none эвристики.
- `app-egui` только регистрирует HTTP/FTP providers и передаёт их IDs сервисному adapter-у.

## Инварианты target-а и secrets

- `FtpRequestTarget` хранит exact locator отдельно от decoded command values.
- Path, username и password percent-decode-ятся ровно один раз как UTF-8 перед FTP commands.
- Control characters (включая CR/LF/NUL), invalid UTF-8 и root-only/missing file path отклоняются до network I/O.
- Debug/Display/errors не раскрывают userinfo, path или query; exact locator доступен только concrete request boundary.

## Data channel и cancellation

- IPv4 PASV host из server reply не считается доверенным: `set_passive_nat_workaround(true)` подменяет его адресом control peer, закрывая FTP bounce/SSRF.
- IPv6 использует EPSV (`Mode::ExtendedPassive`), где отдельный data host отсутствует.
- Custom `passive_stream_builder` задаёт bounded connect/read/write timeouts data socket-а.
- После `RETR` read timeout переключается на короткий 100 ms poll; `read_with_cancellation` проверяет cancellation между polls и сохраняет общий configured read deadline.

## Transfer lifecycle и seekability

- REST capability остаётся exact tri-state: `Supported`, `Unsupported`, `Unknown`; seekable source создаётся только при `Supported`.
- Успешный `finalize_retr` выполняется только после EOF data channel-а.
- Seek/drop/cancel незавершённого RETR сначала закрывает data, затем отбрасывает control session; код не требует ложного `226`/`250` от оборванного transfer-а. Новый seek открывает независимую control/data session и применяет REST offset.
- `FtpPreparedOpen::into_seekable` и `into_streaming` принимают caller-owned `CancellationToken`; hidden never-cancelled open path удалён (кроме trait-level `seek`, у которого cancellation отсутствует, но операции bounded timeouts).

## Расположение focused tests

- Target parsing/security: `crates/source-core/src/ftp_policy.rs`.
- Session/data-channel regression fixtures: `crates/source-core/src/ftp_session_tests.rs`.
- Маленькая data-socket policy: `crates/source-core/src/ftp_session_data_channel.rs`.
- Provider boundary: `crates/web-media-ftp/src/tests.rs`.
- Mixed HTTP+FTP component routing: `crates/service-ytdlp/src/candidate/tests.rs`.

## Проверенные регрессии

- PASV reply с чужим IPv4 host остаётся подключённым к control peer.
- Зависший data read отменяется без ожидания полного read timeout.
- Seek после partial RETR не ломается на server `426` старой сессии.
- Percent-encoded Unicode path/credentials доходят decoded, а diagnostics остаются redacted.
- Explicit FTPS и cleartext/FTPS mismatch покрыты тестами.
- Strict clippy, workspace check, focused tests, app tests, rustdoc `-D warnings` и refactor guardrails проходят.


## Follow-up 2026-08-15: FTP Ogg/Vorbis row 10 и yt-dlp impersonation

- Root cause пользовательской ошибки `stderr скрыт, 219 bytes`: candidate/topology argv безусловно добавлял `--extractor-args generic:impersonate`; stock yt-dlp 2026.07.04 отвергал native FTP до извлечения, если curl-cffi impersonation backend отсутствовал.
- `service-ytdlp` теперь выводит typed `GenericExtractorImpersonation` из `YtDlpInputScheme`: HTTP(S) сохраняет обязательный generic impersonation path, native FTP/FTPS не получает HTTP-only process policy. Embed recovery остаётся HTTP и явно требует impersonation.
- Pinned yt-dlp добавляет к FTP format rows ambient browser headers (`User-Agent`, `Accept`, `Accept-Encoding`, `Accept-Language`, `Sec-Fetch-Mode`). FTP projection игнорирует только этот закрытый case-insensitive allowlist; cookies, range, Authorization и любые custom/unknown HTTP headers по-прежнему fail closed.
- Для audio-only content-probed row (`vcodec: "none"`) video placeholders вроде `vbr: 0` больше не нормализуются как video hints. Если video declared/unknown, прежняя строгая валидация hints сохраняется.
- Regression coverage: exact candidate/topology argv tests; descriptor test с реальными ambient headers и `vbr: 0`; hermetic vertical app test `content_probe_tests::ftp_vorbis::ftp_ogg_with_ambient_http_headers_reaches_production_pcm` через fake yt-dlp -> loopback passive FTP/RETR -> production FTP provider -> Symphonia Ogg -> production Vorbis decoder -> non-empty PCM.
- Exact public acceptance source `ftp://ftp.gnu.org/video/Stephen_Fry-Happy_Birthday_GNU-100kbit_vorbis.ogg` отдельно прошёл тем же production child path до PCM (28.80 s).
- Проверки: `service-ytdlp` 140 tests; `app-egui --no-default-features` 948 tests; оба clippy с `-D warnings`; workspace check; refactor guardrails; fmt; diff check; release build.
