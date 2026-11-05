//! Тесты каталога: разбор, круговая проверка, отказы и (вручную) настоящая генерация ffmpeg.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use super::*;

/// Каталог из одного segmented-ролика и одного файла; файлы создаёт тест, чтобы проверка их находила.
fn write_sample_catalog(root: &Path) -> CatalogFile {
    fs::create_dir_all(root.join("clip/video")).unwrap();
    fs::write(root.join("clip/video/init.mp4"), b"init").unwrap();
    fs::write(root.join("clip/video/seg-00001.m4s"), b"segment-one").unwrap();
    fs::write(root.join("clip.vtt"), b"WEBVTT\n").unwrap();
    let json = serde_json::json!({
        "version": 1,
        "assets": {
            "clip": {
                "kind": "segmented", "duration_s": 2.0,
                "tracks": [{
                    "id": "video", "kind": "video", "mime": "video/mp4; codecs=\"avc1.64001e\"",
                    "width": 640, "height": 360, "init": "clip/video/init.mp4",
                    "segments": [{"url": "clip/video/seg-00001.m4s", "start_s": 0.0, "duration_s": 2.0}]
                }]
            },
            "subs": {"kind": "text", "url": "clip.vtt", "mime": "text/vtt"}
        }
    });
    fs::write(root.join(CATALOG_FILE_NAME), json.to_string()).unwrap();
    serde_json::from_value(json).unwrap()
}

#[test]
fn catalog_round_trips_and_resolves_files() {
    let dir = tempfile::tempdir().unwrap();
    let expected = write_sample_catalog(dir.path());

    let catalog = MediaCatalog::load(dir.path()).unwrap();

    assert_eq!(catalog.file, expected);
    let Some(Asset::Segmented { tracks, .. }) = catalog.asset("clip") else {
        panic!("clip must be a segmented asset");
    };
    assert_eq!(tracks[0].kind, TrackKind::Video);
    assert_eq!(tracks[0].segments[0].duration_s, 2.0);
    // Сериализация обратно даёт тот же каталог (формат не теряет поля).
    let reparsed: CatalogFile =
        serde_json::from_str(&serde_json::to_string(&catalog.file).unwrap()).unwrap();
    assert_eq!(reparsed, catalog.file);
    assert_eq!(
        fs::read(catalog.resolve("clip/video/seg-00001.m4s").unwrap()).unwrap(),
        b"segment-one"
    );
    assert!(catalog.asset("absent").is_none());
}

#[test]
fn resolve_refuses_traversal_and_missing_files() {
    let dir = tempfile::tempdir().unwrap();
    let parent_secret = dir.path().join("secret.txt");
    fs::write(&parent_secret, b"secret").unwrap();
    let media_root = dir.path().join("media");
    write_sample_catalog(&media_root);
    let catalog = MediaCatalog::load(&media_root).unwrap();

    assert!(catalog.resolve("../secret.txt").is_none());
    assert!(catalog.resolve("clip/../../secret.txt").is_none());
    assert!(catalog.resolve("/etc/passwd").is_none());
    assert!(catalog.resolve("clip\\video\\init.mp4").is_none());
    assert!(catalog.resolve("clip/video/nope.m4s").is_none());
    assert!(
        catalog.resolve("clip/video").is_none(),
        "directory is not a file"
    );
}

#[test]
fn load_fails_when_a_referenced_file_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    write_sample_catalog(dir.path());
    fs::remove_file(dir.path().join("clip/video/seg-00001.m4s")).unwrap();

    let error = MediaCatalog::load(dir.path()).unwrap_err();

    assert!(
        matches!(&error, MediaError::MissingFile { asset, url } if asset == "clip" && url == "clip/video/seg-00001.m4s"),
        "unexpected error: {error}"
    );
}

#[test]
fn load_fails_on_unsupported_version_and_unsafe_url() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join(CATALOG_FILE_NAME),
        r#"{"version": 2, "assets": {}}"#,
    )
    .unwrap();
    assert!(matches!(
        MediaCatalog::load(dir.path()).unwrap_err(),
        MediaError::UnsupportedVersion {
            found: 2,
            expected: 1
        }
    ));

    fs::write(
        dir.path().join(CATALOG_FILE_NAME),
        r#"{"version": 1, "assets": {"x": {"kind": "text", "url": "../outside.vtt", "mime": "text/vtt"}}}"#,
    )
    .unwrap();
    assert!(matches!(
        MediaCatalog::load(dir.path()).unwrap_err(),
        MediaError::UnsafeUrl { .. }
    ));

    fs::write(dir.path().join(CATALOG_FILE_NAME), "not json").unwrap();
    assert!(matches!(
        MediaCatalog::load(dir.path()).unwrap_err(),
        MediaError::CatalogParse { .. }
    ));
}

/// Дорогой тест (около 15 с, нужен ffmpeg): запускает настоящий скрипт и проверяет, что
/// сегменты каждого сегментированного ролика СКЛЕИВАЮТСЯ с init и декодируются ffprobe-ом.
/// Запуск: `cargo test -p browser-bridge-bench generates_real_media -- --ignored --nocapture`.
#[test]
#[ignore = "runs ffmpeg for ~15 s; run explicitly"]
fn generates_real_media_and_segments_decode() {
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    generate_media_into(dir.path()).unwrap();
    eprintln!("media generated in {:?}", started.elapsed());

    let catalog = MediaCatalog::load(dir.path()).unwrap();
    for required in [
        "h264-aac",
        "vp9-opus",
        "av1-aac",
        "muxed-h264-aac",
        "dub-a",
        "dub-b",
        "progressive-h264-aac",
        "progressive-long-h264-aac",
        "ad-preroll",
        "ad-second",
        "hls-h264-aac",
        "clearkey-h264",
        "subtitles-en",
        "subtitles-uk",
    ] {
        assert!(
            catalog.asset(required).is_some(),
            "asset {required} is missing"
        );
    }
    let Some(Asset::Segmented { tracks, .. }) = catalog.asset("h264-aac") else {
        panic!("h264-aac must be segmented");
    };
    let starts = |index: usize| {
        tracks[index]
            .segments
            .iter()
            .map(|segment| segment.start_s)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        starts(0),
        starts(1),
        "video qualities must share segment boundaries"
    );
    assert_eq!(tracks[0].segments.len(), 15);
    let Some(Asset::Clearkey {
        key_id_hex,
        key_hex,
        ..
    }) = catalog.asset("clearkey-h264")
    else {
        panic!("clearkey-h264 must be a clearkey asset");
    };
    assert_eq!((key_id_hex.len(), key_hex.len()), (32, 32));

    let mut decoded_tracks = 0;
    for (asset_id, asset) in &catalog.file.assets {
        for track in asset.tracks() {
            let frames = decode_first_segment(&catalog, &track.init, &track.segments[0].url);
            assert!(
                frames > 0,
                "{asset_id}/{} first segment decoded no packets",
                track.id
            );
            decoded_tracks += 1;
        }
    }
    assert!(
        decoded_tracks >= 10,
        "expected many tracks, decoded {decoded_tracks}"
    );
}

/// Склеивает init и первый сегмент и считает пакеты, которые видит ffprobe.
fn decode_first_segment(catalog: &MediaCatalog, init_url: &str, segment_url: &str) -> usize {
    let init = fs::read(catalog.resolve(init_url).unwrap()).unwrap();
    let segment = fs::read(catalog.resolve(segment_url).unwrap()).unwrap();
    let extension = Path::new(init_url).extension().unwrap().to_str().unwrap();
    let joined = tempfile::Builder::new()
        .suffix(&format!(".{extension}"))
        .tempfile()
        .unwrap();
    fs::write(joined.path(), [init, segment].concat()).unwrap();
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "packet=pts_time",
            "-of",
            "csv=p=0",
        ])
        .arg(joined.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .count()
}
