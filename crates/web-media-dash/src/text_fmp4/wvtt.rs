//! Сэмплы `wvtt` (ISO/IEC 14496-30) -> текст WebVTT.
//!
//! Сэмпл содержит боксы `vttc` (одна реплика: `iden` - идентификатор, `sttg` - настройки,
//! `payl` - текст, `ctim` - подсказка времени для разрезанных реплик, здесь не нужна) либо
//! один `vtte` (пустой интервал: реплик нет). Время начала реплики - время сэмпла, конец -
//! время сэмпла плюс длительность; своих меток внутри реплики у `wvtt` нет.
//!
//! Результат - обычный WebVTT-документ с метками media timeline дорожки (без правки PTO):
//! дальше его разбирает общий парсер WebVTT, а сдвиг на шкалу плеера применяет владелец.

use std::fmt::Write as _;

use super::boxes::BoxIter;
use super::{TextFmp4Error, TextSample};

/// Строит документ WebVTT из сэмплов `wvtt` дорожки с таймскейлом `timescale`.
pub(crate) fn webvtt_document_from_samples(
    samples: &[TextSample],
    timescale: u32,
) -> Result<String, TextFmp4Error> {
    let mut document = String::from("WEBVTT\n\n");
    for sample in samples {
        let start_micros = ticks_to_micros(sample.decode_time, timescale)?;
        let end_ticks = sample
            .decode_time
            .checked_add(sample.duration)
            .ok_or(TextFmp4Error::Overflow)?;
        let end_micros = ticks_to_micros(end_ticks, timescale)?;
        for cue in cues_of_sample(&sample.data)? {
            // Реплика без текста ничего не показывает: пропускаем (как и `vtte`).
            if cue.payload.is_empty() {
                continue;
            }
            if let Some(identifier) = &cue.identifier {
                // Запись в String не может завершиться ошибкой.
                let _ = writeln!(document, "{identifier}");
            }
            let _ = write!(
                document,
                "{} --> {}",
                format_timestamp(start_micros),
                format_timestamp(end_micros)
            );
            if let Some(settings) = &cue.settings {
                let _ = write!(document, " {settings}");
            }
            let _ = write!(document, "\n{}\n\n", cue.payload);
        }
    }
    Ok(document)
}

/// Реплика одного бокса `vttc`.
struct WvttCue {
    identifier: Option<String>,
    settings: Option<String>,
    payload: String,
}

/// Реплики одного сэмпла (`vtte` и неизвестные боксы дают пустой список).
fn cues_of_sample(sample: &[u8]) -> Result<Vec<WvttCue>, TextFmp4Error> {
    let mut cues = Vec::new();
    for top_level in BoxIter::new(sample, 0) {
        let top_level = top_level?;
        if &top_level.kind != b"vttc" {
            continue;
        }
        let mut cue = WvttCue {
            identifier: None,
            settings: None,
            payload: String::new(),
        };
        for field in BoxIter::new(top_level.payload, 0) {
            let field = field?;
            let text = String::from_utf8_lossy(field.payload);
            match &field.kind {
                b"iden" => {
                    cue.identifier = Some(single_line(&text)).filter(|line| !line.is_empty())
                }
                b"sttg" => cue.settings = Some(single_line(&text)).filter(|line| !line.is_empty()),
                b"payl" => cue.payload = sanitize_payload(&text),
                _ => {}
            }
        }
        cues.push(cue);
    }
    Ok(cues)
}

/// Строка без переводов строки и без `-->` (иначе сломала бы разметку документа).
fn single_line(text: &str) -> String {
    let joined = text
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    joined.replace("-->", "--&gt;")
}

/// Текст реплики: единые переводы строк, без пустых строк (они закончили бы реплику) и без `-->`.
fn sanitize_payload(text: &str) -> String {
    text.split(['\r', '\n'])
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .replace("-->", "--&gt;")
}

/// Единицы таймскейла -> микросекунды (округление вниз).
fn ticks_to_micros(ticks: u64, timescale: u32) -> Result<u64, TextFmp4Error> {
    if timescale == 0 {
        return Err(TextFmp4Error::ZeroTimescale);
    }
    let micros = u128::from(ticks) * 1_000_000 / u128::from(timescale);
    u64::try_from(micros).map_err(|_| TextFmp4Error::Overflow)
}

/// Метка WebVTT `HH:MM:SS.mmm` (часов может быть больше двух цифр).
fn format_timestamp(micros: u64) -> String {
    let total_millis = micros / 1_000;
    let millis = total_millis % 1_000;
    let total_seconds = total_millis / 1_000;
    let seconds = total_seconds % 60;
    let total_minutes = total_seconds / 60;
    let minutes = total_minutes % 60;
    let hours = total_minutes / 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}
