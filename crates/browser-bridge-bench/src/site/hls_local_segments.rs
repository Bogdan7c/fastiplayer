//! Маршрут `/cdn/hls-local-segments/<плейлист>` (сессия 10a2): HLS-плейлист, который «сайт из
//! интернета» отдаёт публично, но ЧАСТЬ сегментов в нём указана абсолютными адресами на локальный
//! адрес (`http://127.0.0.1:<порт стенда>/media-local-target/…`). Это вектор атаки решения 199 для
//! HLS: корневой плейлист публичный, проверка адреса его пропускает, а каждый следующий ресурс
//! обязан проходить ту же проверку. Порт известен только во время работы, поэтому плейлист
//! строится на лету.
//!
//! Зачем «часть»: Chromium не начинает играть родной HLS под публичным именем, если ВСЕ сегменты
//! на 127.0.0.1, и передавать было бы нечего. Первые [`PUBLIC_SEGMENT_COUNT`] сегментов остаются
//! публичными (корневой путь `/media/…` на том же хосте, что и страница), остальные уходят на
//! локальный адрес.
//!
//! Локальные сегменты лежат под отдельным префиксом `/media-local-target/` (те же файлы, что и
//! `/media/`): по журналу сервера публичные и локальные запросы различимы независимо от `Host`.
//!
//! Правила переписывания (чистая функция [`rewrite_media_playlist`]):
//! - строки-комментарии и теги не трогаем;
//! - строка-адрес на другой плейлист (`.m3u8`) остаётся относительной: вложенный плейлист снова
//!   придёт на этот же маршрут и будет переписан;
//! - остальные строки-адреса (сегменты) нумеруются с нуля: номера до порога — `/media/<каталог>/…`,
//!   начиная с порога — `<локальный origin>/media-local-target/<каталог>/…`.

use super::http::{Body, Response};
use super::routes::RouteContext;
use crate::media::safe_path::resolve_existing_file;

/// Префикс маршрута.
pub(super) const ROUTE_PREFIX: &str = "/cdn/hls-local-segments/";
/// Сколько первых сегментов остаются публичными (по 2 с каждый): браузеру хватает, чтобы начать игру.
pub(super) const PUBLIC_SEGMENT_COUNT: usize = 3;
/// Префикс, под которым сервер отдаёт «локальные» сегменты (те же файлы, что под `/media/`).
pub(super) const LOCAL_TARGET_PREFIX: &str = "/media-local-target/";
/// Расширение плейлиста: только такие файлы маршрут отдаёт.
const PLAYLIST_EXTENSION: &str = ".m3u8";
/// Тип содержимого плейлиста.
const PLAYLIST_CONTENT_TYPE: &str = "application/vnd.apple.mpegurl";

/// Отвечает на запрос плейлиста `relative` (путь внутри каталога медиа): 404, если это не
/// существующий плейлист; иначе плейлист с абсолютными локальными адресами сегментов.
pub(super) fn serve(context: &RouteContext, relative: &str) -> Response {
    if !relative.ends_with(PLAYLIST_EXTENSION) {
        return Response::text(404, "only playlists are served here");
    }
    let Some(path) = resolve_existing_file(&context.shared.media.root, relative) else {
        return Response::text(404, "not found");
    };
    let original = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => return Response::text(500, &format!("cannot read the playlist: {error}")),
    };
    let playlist_directory = relative
        .rsplit_once('/')
        .map_or("", |(directory, _)| directory);
    let rewritten = rewrite_media_playlist(&original, &context.own_origin, playlist_directory);
    Response::new(200)
        .with_header("Content-Type", PLAYLIST_CONTENT_TYPE)
        .with_header("Cache-Control", "no-store")
        .with_header("Access-Control-Allow-Origin", "*")
        .with_body(Body::Bytes(rewritten.into_bytes()))
}

/// Делит сегменты на публичные и локальные: `local_origin` — настоящий адрес сервера стенда,
/// `playlist_directory` — каталог плейлиста внутри медиа (без слэшей по краям).
pub(super) fn rewrite_media_playlist(
    playlist: &str,
    local_origin: &str,
    playlist_directory: &str,
) -> String {
    let directory = if playlist_directory.is_empty() {
        String::new()
    } else {
        format!("{playlist_directory}/")
    };
    let mut segment_number = 0usize;
    let mut rewritten: Vec<String> = playlist
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            let is_segment_uri = !trimmed.is_empty()
                && !trimmed.starts_with('#')
                && !trimmed.ends_with(PLAYLIST_EXTENSION);
            if !is_segment_uri {
                return line.to_owned();
            }
            let number = segment_number;
            segment_number += 1;
            if number < PUBLIC_SEGMENT_COUNT {
                format!("/media/{directory}{trimmed}")
            } else {
                format!("{local_origin}{LOCAL_TARGET_PREFIX}{directory}{trimmed}")
            }
        })
        .collect();
    // Плейлист по стандарту заканчивается переводом строки.
    rewritten.push(String::new());
    rewritten.join("\n")
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    use super::super::{StandSite, StandSiteConfig};
    use super::*;

    const LOCAL_ORIGIN: &str = "http://127.0.0.1:4321";
    const MEDIA_PLAYLIST: &str = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2.000000,\nseg-00000.ts\n#EXTINF:2.000000,\nseg-00001.ts\n#EXT-X-ENDLIST\n";

    #[test]
    fn first_segments_stay_public_and_the_rest_point_to_the_local_target() {
        let rewritten = rewrite_media_playlist(MEDIA_PLAYLIST, LOCAL_ORIGIN, "hls");
        // В плейлисте теста два сегмента: оба ниже порога, значит публичные.
        assert_eq!(
            rewritten,
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2.000000,\n/media/hls/seg-00000.ts\n#EXTINF:2.000000,\n/media/hls/seg-00001.ts\n#EXT-X-ENDLIST\n"
        );
        let long: String = (0..PUBLIC_SEGMENT_COUNT + 2)
            .map(|index| format!("#EXTINF:2.0,\nseg-{index:05}.ts\n"))
            .collect();
        let rewritten = rewrite_media_playlist(&long, LOCAL_ORIGIN, "hls");
        let uris: Vec<&str> = rewritten.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(
            uris[PUBLIC_SEGMENT_COUNT - 1],
            format!("/media/hls/seg-{:05}.ts", PUBLIC_SEGMENT_COUNT - 1)
        );
        assert_eq!(
            uris[PUBLIC_SEGMENT_COUNT],
            format!("{LOCAL_ORIGIN}/media-local-target/hls/seg-{PUBLIC_SEGMENT_COUNT:05}.ts")
        );
        assert_eq!(uris.len(), PUBLIC_SEGMENT_COUNT + 2);
    }

    #[test]
    fn a_nested_playlist_line_stays_relative_and_is_not_counted_as_a_segment() {
        let master = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nstream.m3u8\n";
        assert_eq!(rewrite_media_playlist(master, LOCAL_ORIGIN, "hls"), master);
    }

    #[test]
    fn a_playlist_at_the_media_root_gets_no_double_slash() {
        let rewritten = rewrite_media_playlist("#EXTM3U\nseg.ts\n", LOCAL_ORIGIN, "");
        assert!(rewritten.contains("\n/media/seg.ts\n"), "{rewritten}");
    }

    /// Сырой GET на `origin`; возвращает статус и тело.
    fn get(origin: &str, path: &str) -> (u16, String) {
        let host_port = origin.strip_prefix("http://").expect("http origin");
        let mut stream = TcpStream::connect(host_port).expect("connect");
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n"
        )
        .expect("send");
        let mut raw = String::new();
        stream.read_to_string(&mut raw).expect("read");
        let status = raw
            .split(' ')
            .nth(1)
            .and_then(|code| code.parse().ok())
            .expect("status code");
        let body = raw.split_once("\r\n\r\n").map_or("", |(_, body)| body);
        (status, body.to_owned())
    }

    #[test]
    fn the_route_serves_the_playlist_with_the_real_origin_and_logs_the_request() {
        let dir = tempfile::tempdir().expect("temp dir");
        let media_root = dir.path().join("media");
        std::fs::create_dir_all(media_root.join("hls")).expect("media dir");
        std::fs::write(media_root.join("hls/stream.m3u8"), MEDIA_PLAYLIST).expect("playlist");
        std::fs::write(media_root.join("hls/seg-00000.ts"), b"x").expect("segment");
        std::fs::write(
            media_root.join("catalog.json"),
            r#"{"version":1,"assets":{"clip":{"kind":"hls","url":"hls/stream.m3u8","duration_s":4.0}}}"#,
        )
        .expect("catalog");
        let site_root = dir.path().join("site");
        std::fs::create_dir_all(&site_root).expect("site dir");
        let media = crate::media::MediaCatalog::load(&media_root).expect("catalog loads");
        let site = StandSite::start(StandSiteConfig { site_root, media }).expect("site starts");
        let origin = site.main_origin();

        let (status, body) = get(&origin, "/cdn/hls-local-segments/hls/stream.m3u8");
        assert_eq!(status, 200);
        assert!(body.contains("\n/media/hls/seg-00001.ts\n"), "{body}");
        // Сам файл на диске не изменился: переписывается только ответ.
        let on_disk = std::fs::read_to_string(media_root.join("hls/stream.m3u8")).expect("read");
        assert_eq!(on_disk, MEDIA_PLAYLIST);
        // Не плейлист и несуществующий плейлист — 404; небезопасный путь сюда не проходит.
        assert_eq!(
            get(&origin, "/cdn/hls-local-segments/hls/seg-00000.ts").0,
            404
        );
        assert_eq!(get(&origin, "/cdn/hls-local-segments/hls/none.m3u8").0, 404);
        assert_eq!(get(&origin, "/cdn/hls-local-segments/../x.m3u8").0, 404);
        // Запрос плейлиста виден сценарию в журнале.
        let logged = site.media_requests();
        assert!(logged.iter().any(|request| {
            request.path == "/cdn/hls-local-segments/hls/stream.m3u8" && request.status == 200
        }));
    }
}
