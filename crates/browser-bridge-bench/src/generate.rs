//! `generate-recording`: синтетическая запись потока страницы из ассета стенда.
//! Только ввод-вывод (кэш медиа, запись файла); сама сборка записи — в `recording::synthesize`.

use browser_bridge_bench::media::ensure_media;
use browser_bridge_bench::recording::{Recording, SynthesisOptions, synthesize_from_asset};
use browser_bridge_bench::scenarios::RunSettings;

use crate::cli::GenerationOptions;

/// Создаёт файл записи и возвращает строку для печати (путь, число событий, байты склада).
/// Записанный файл тут же разбирается обратно: генератор не выдаёт файл, который не читается.
pub fn generate_recording(
    settings: &RunSettings,
    options: &GenerationOptions,
) -> Result<String, String> {
    let catalog = ensure_media(&settings.media_cache).map_err(|error| format!("media: {error}"))?;
    let bytes = synthesize_from_asset(&catalog, &options.asset, SynthesisOptions::default())
        .map_err(|error| error.to_string())?;
    std::fs::write(&options.output, &bytes)
        .map_err(|error| format!("cannot write {}: {error}", options.output.display()))?;
    let written = Recording::parse(&bytes)
        .map_err(|error| format!("generated file is unreadable: {error}"))?;
    Ok(format!(
        "wrote {}: {} events, {} payload bytes, hand-off at {:?} ms, ends at {} ms",
        options.output.display(),
        written.events().len(),
        written.store_length(),
        written.handoff_began_at_ms(),
        written.duration_ms()
    ))
}
