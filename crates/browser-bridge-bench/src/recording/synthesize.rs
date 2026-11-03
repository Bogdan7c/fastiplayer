//! Синтетическая запись из fMP4-ассета стенда: «как будто страница играла этот ролик».
//!
//! Нужна, чтобы путь повтора (`replay`) можно было доказать до кадра в плеере без записи с
//! настоящего сайта. Страница в синтетической записи ведёт себя как обычный MSE-плеер:
//! создаёт MediaSource, привязывает его к элементу, добавляет SourceBuffer на каждую дорожку,
//! докладывает init-сегменты, запускает `play()` и подкачивает сегменты с опережением
//! `buffer_ahead_s`; в `handoff_at_ms` вызывает передачу плееру, в конце закрывает поток.

use std::path::Path;

use serde_json::json;

use super::error::RecordingError;
use super::format::MAIN_CONTEXT;
use super::write::RecordingBuilder;
use crate::media::{Asset, MediaCatalog, Track, TrackKind};

/// Идентификатор MediaSource в синтетической записи.
const MEDIA_SOURCE_ID: &str = "ms-1";
/// Идентификатор видеоэлемента.
const ELEMENT_ID: &str = "el-1";
/// Origin «страницы-источника» синтетической записи (ничей, только метка).
const SYNTHETIC_ORIGIN: &str = "http://synthetic.invalid";
/// Через сколько мс после появления SourceBuffer страница докладывает init-сегменты.
const INIT_DELAY_MS: f64 = 50.0;
/// Когда страница вызывает `play()` (мс от старта записи).
const PLAY_AT_MS: f64 = 200.0;
/// Сколько мс страница тратит на один `appendBuffer` (событие `update_end` после `append`).
const UPDATE_DURATION_MS: f64 = 10.0;
/// Пауза между последним сегментом и `end_of_stream`, мс.
const END_OF_STREAM_DELAY_MS: f64 = 100.0;

/// Параметры синтетической записи.
#[derive(Debug, Clone, Copy)]
pub struct SynthesisOptions {
    /// Когда страница отдаёт вкладку плееру (мс от старта записи).
    pub handoff_at_ms: f64,
    /// На сколько секунд вперёд от позиции воспроизведения страница держит буфер.
    pub buffer_ahead_s: f64,
}

impl Default for SynthesisOptions {
    fn default() -> Self {
        Self {
            handoff_at_ms: 3000.0,
            buffer_ahead_s: 4.0,
        }
    }
}

/// Дорожка ассета вместе с идентификатором её SourceBuffer.
struct PlannedTrack<'a> {
    source_buffer_id: String,
    track: &'a Track,
}

/// Собирает файл `*.fprec` из сегментированного ассета каталога.
pub fn synthesize_from_asset(
    catalog: &MediaCatalog,
    asset_id: &str,
    options: SynthesisOptions,
) -> Result<Vec<u8>, RecordingError> {
    let unsuitable = |reason: &str| RecordingError::UnsuitableAsset {
        asset: asset_id.to_owned(),
        reason: reason.to_owned(),
    };
    let asset = catalog
        .asset(asset_id)
        .ok_or_else(|| unsuitable("not in the catalog"))?;
    let Asset::Segmented { duration_s, .. } = asset else {
        return Err(unsuitable("only segmented fMP4 assets are supported"));
    };
    let planned = plan_tracks(asset);
    if planned.is_empty() {
        return Err(unsuitable("no video or audio tracks"));
    }

    let mut builder = RecordingBuilder::new("chrome", SYNTHETIC_ORIGIN);
    let at = |builder: &mut RecordingBuilder, t_ms: f64, op: &str, fields: serde_json::Value| {
        builder.event(t_ms, MAIN_CONTEXT, op, fields);
    };
    // Страница создаёт MediaSource и привязывает к элементу (как `video.src = URL.createObjectURL`).
    at(
        &mut builder,
        0.0,
        "media_source_created",
        json!({ "ms": MEDIA_SOURCE_ID }),
    );
    at(
        &mut builder,
        1.0,
        "media_source_object_url",
        json!({ "ms": MEDIA_SOURCE_ID }),
    );
    at(
        &mut builder,
        2.0,
        "element_source",
        json!({ "el": ELEMENT_ID, "kind": "media_source_url", "ms": MEDIA_SOURCE_ID }),
    );
    at(
        &mut builder,
        20.0,
        "source_open",
        json!({ "ms": MEDIA_SOURCE_ID }),
    );
    for planned_track in &planned {
        at(
            &mut builder,
            30.0,
            "add_source_buffer",
            json!({
                "ms": MEDIA_SOURCE_ID,
                "sb": planned_track.source_buffer_id,
                "mime": planned_track.track.mime,
            }),
        );
    }
    at(
        &mut builder,
        40.0,
        "duration",
        json!({ "ms": MEDIA_SOURCE_ID, "value": duration_s }),
    );
    // Init-сегменты.
    for planned_track in &planned {
        let bytes = read_asset_file(catalog, &planned_track.track.init)?;
        push_append(&mut builder, 30.0 + INIT_DELAY_MS, planned_track, &bytes);
    }
    at(
        &mut builder,
        PLAY_AT_MS,
        "element_play",
        json!({ "el": ELEMENT_ID }),
    );

    // Сегменты: каждый докладывается за `buffer_ahead_s` до своего начала на шкале ролика.
    let mut last_append_ms = PLAY_AT_MS;
    let mut scheduled: Vec<(f64, &PlannedTrack<'_>, &str)> = Vec::new();
    for planned_track in &planned {
        for segment in &planned_track.track.segments {
            let due = PLAY_AT_MS + (segment.start_s - options.buffer_ahead_s).max(0.0) * 1000.0;
            scheduled.push((due, planned_track, segment.url.as_str()));
        }
    }
    // Стабильная сортировка: при равном времени порядок дорожек сохраняется.
    scheduled.sort_by(|left, right| left.0.total_cmp(&right.0));
    for (due, planned_track, url) in scheduled {
        let bytes = read_asset_file(catalog, url)?;
        push_append(&mut builder, due, planned_track, &bytes);
        last_append_ms = last_append_ms.max(due + UPDATE_DURATION_MS);
    }
    // Передача плееру — вставляем на своё место по времени (сортировка событий ниже).
    at(
        &mut builder,
        options.handoff_at_ms,
        "handoff_began",
        json!({ "el": ELEMENT_ID }),
    );
    at(
        &mut builder,
        // Поток не закрывается раньше передачи: иначе повтор не проверил бы живой хвост.
        last_append_ms.max(options.handoff_at_ms) + END_OF_STREAM_DELAY_MS,
        "end_of_stream",
        json!({ "ms": MEDIA_SOURCE_ID }),
    );
    builder.sort_events_by_time();
    builder.finish()
}

/// Выбирает дорожки: первая видео- (или совмещённая) и первая звуковая.
fn plan_tracks(asset: &Asset) -> Vec<PlannedTrack<'_>> {
    let tracks = asset.tracks();
    let main = tracks
        .iter()
        .find(|track| matches!(track.kind, TrackKind::Video | TrackKind::Muxed));
    let audio = tracks.iter().find(|track| track.kind == TrackKind::Audio);
    [main, audio]
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, track)| PlannedTrack {
            source_buffer_id: format!("sb-{}", index + 1),
            track,
        })
        .collect()
}

/// `append` и сразу за ним `update_end` страницы (она ждёт его перед следующим вызовом).
fn push_append(
    builder: &mut RecordingBuilder,
    t_ms: f64,
    planned: &PlannedTrack<'_>,
    bytes: &[u8],
) {
    builder.append(
        t_ms,
        MAIN_CONTEXT,
        MEDIA_SOURCE_ID,
        &planned.source_buffer_id,
        bytes,
    );
    builder.event(
        t_ms + UPDATE_DURATION_MS,
        MAIN_CONTEXT,
        "update_end",
        json!({ "ms": MEDIA_SOURCE_ID, "sb": planned.source_buffer_id }),
    );
}

fn read_asset_file(catalog: &MediaCatalog, url: &str) -> Result<Vec<u8>, RecordingError> {
    let path = catalog
        .resolve(url)
        .ok_or_else(|| RecordingError::UnsuitableAsset {
            asset: url.to_owned(),
            reason: "file from the catalog is missing on disk".to_owned(),
        })?;
    read_file(&path)
}

fn read_file(path: &Path) -> Result<Vec<u8>, RecordingError> {
    std::fs::read(path).map_err(|source| RecordingError::Io {
        path: path.to_path_buf(),
        source,
    })
}
