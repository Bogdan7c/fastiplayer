//! Сканирование системных шрифтов и построение индекса покрытия.
//!
//! Это единственное место, где библиотека читает все системные файлы, и делает она это один
//! раз, в явном шаге [`SubtitleFontLibrary::load_system_fonts`] (вызывающий запускает его в
//! фоне). Взамен поиск глифа по системным шрифтам на пути рендера работает только с памятью:
//! для каждого лица хранится [`GlyphCoverage`] (диапазоны кодпоинтов из `cmap`), а файл
//! открывается лишь у выбранного лица.
//!
//! Стоимость: чтение `cmap` каждого лица (mmap файла + обход отображений). Память индекса
//! и время скана попадают в [`SystemFontScanReport`] и [`super::FontLibraryStatistics`].
//! Замер (release, разработческая машина с 982 лицами в 304 семействах, файлы в кэше ОС):
//! около 90 мс на весь скан (включая каталогизацию `fontdb`) и около 1,7 МиБ индекса
//! (порядка 1,8 КиБ на лицо). Скан с холодным кэшем диска дольше, поэтому его запускают
//! в фоне, а не на пути рендера.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use skrifa::{FontRef, MetadataProvider};

use super::coverage::GlyphCoverage;
use super::face::{FaceOrigin, FaceSource, LoadedFace, SystemFileKey, parse_and_inspect};
use super::library::{SubtitleFontLibrary, SystemFontScanReport, SystemFontScanStatus};
use super::metrics::compute_face_metrics;
use super::style::{FontStyle, FontWeightClass};

/// Лицо, найденное `fontdb`, до анализа файла.
struct DiscoveredFace {
    database_id: fontdb::ID,
    file_key: SystemFileKey,
    collection_index: u32,
    family_names: Vec<String>,
    weight: FontWeightClass,
    style: FontStyle,
}

impl SubtitleFontLibrary {
    /// Явный IO-шаг: сканирует системные каталоги шрифтов (через `fontdb`) и строит индекс
    /// покрытия кодпоинтов. Повторный вызов сканирование не повторяет.
    ///
    /// Ошибки отдельных файлов `fontdb` пишет в лог и пропускает; лица, которые не читаются или
    /// не имеют метрик, не регистрируются (счётчик `faces_skipped_unusable`). Паник нет.
    pub fn load_system_fonts(&mut self) -> SystemFontScanReport {
        if self.system_scanned {
            return self.system_scan_report(SystemFontScanStatus::AlreadyScanned, Duration::ZERO);
        }
        let started = Instant::now();
        self.system_database.load_system_fonts();
        self.system_scanned = true;
        let skipped = self.register_database_faces();
        tracing::debug!(
            faces = self.system_face_ids.len(),
            skipped,
            coverage_bytes = self.coverage_index_bytes,
            "system fonts scanned"
        );
        self.system_scan_report_with(skipped, started.elapsed())
    }

    /// Тестовый шов: сканирует один каталог вместо системных (на свежей библиотеке).
    #[cfg(test)]
    pub(crate) fn load_fonts_from_directory(
        &mut self,
        directory: &std::path::Path,
    ) -> SystemFontScanReport {
        let started = Instant::now();
        self.system_database.load_fonts_dir(directory);
        self.system_scanned = true;
        let skipped = self.register_database_faces();
        self.system_scan_report_with(skipped, started.elapsed())
    }

    /// Регистрирует лица базы `fontdb`, ещё не известные библиотеке; возвращает число пропущенных.
    fn register_database_faces(&mut self) -> usize {
        let known: HashSet<fontdb::ID> = self
            .faces
            .iter()
            .filter_map(|face| match &face.source {
                FaceSource::System { database_id, .. } => Some(*database_id),
                _ => None,
            })
            .collect();
        let discovered = self.discover_new_faces(&known);
        let mut skipped = 0;
        for face in discovered {
            let Some(coverage) = self.analyze_face(&face) else {
                skipped += 1;
                self.unusable_faces += 1;
                continue;
            };
            self.coverage_index_bytes = self
                .coverage_index_bytes
                .saturating_add(coverage.memory_bytes());
            let face_id = self.next_face_id();
            self.faces.push(LoadedFace {
                origin: FaceOrigin::System,
                source: FaceSource::System {
                    database_id: face.database_id,
                    file_key: face.file_key,
                    coverage,
                },
                collection_index: face.collection_index,
                family: face.family_names.first().cloned().unwrap_or_default(),
                weight: face.weight,
                style: face.style,
                unusable: false,
            });
            self.system_face_ids.push(face_id);
            for name in &face.family_names {
                self.system_names.add(name, face_id);
            }
        }
        // Отрицательные результаты поиска глифа («нигде нет») могли устареть: системные
        // шрифты теперь участвуют в fallback.
        self.lookup_cache.clear();
        skipped
    }

    /// Лица базы, которых библиотека ещё не видела.
    fn discover_new_faces(&self, known: &HashSet<fontdb::ID>) -> Vec<DiscoveredFace> {
        self.system_database
            .faces()
            .filter(|info| !known.contains(&info.id))
            .map(|info| DiscoveredFace {
                database_id: info.id,
                file_key: match &info.source {
                    fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => {
                        SystemFileKey::Path(path.clone())
                    }
                    fontdb::Source::Binary(_) => SystemFileKey::Face(info.id),
                },
                collection_index: info.index,
                family_names: info.families.iter().map(|(name, _)| name.clone()).collect(),
                weight: FontWeightClass::from_os2_weight(info.weight.0),
                style: match info.style {
                    fontdb::Style::Normal => FontStyle::Normal,
                    fontdb::Style::Italic | fontdb::Style::Oblique => FontStyle::Italic,
                },
            })
            .collect()
    }

    /// Открывает файл лица один раз и строит покрытие; `None` — лицо непригодно.
    fn analyze_face(&self, face: &DiscoveredFace) -> Option<GlyphCoverage> {
        self.counters.note_system_file_open();
        self.system_database
            .with_face_data(face.database_id, |bytes, index| {
                parse_and_inspect(bytes, index, &self.counters, coverage_of_renderable_face)
                    .flatten()
            })
            .flatten()
    }

    fn system_scan_report(
        &self,
        status: SystemFontScanStatus,
        elapsed: Duration,
    ) -> SystemFontScanReport {
        let mut report = self.system_scan_report_with(0, elapsed);
        report.status = status;
        report
    }

    fn system_scan_report_with(&self, skipped: usize, elapsed: Duration) -> SystemFontScanReport {
        let family_names = self.system_names.family_count();
        SystemFontScanReport {
            status: SystemFontScanStatus::Scanned,
            faces_registered: self.system_face_ids.len(),
            family_names,
            faces_skipped_unusable: skipped,
            coverage_index_bytes: self.coverage_index_bytes,
            elapsed,
        }
    }
}

/// Покрытие лица, если у него есть пригодные метрики; иначе `None`.
fn coverage_of_renderable_face(font: &FontRef<'_>) -> Option<GlyphCoverage> {
    if !compute_face_metrics(font)?.is_renderable() {
        return None;
    }
    Some(GlyphCoverage::from_mappings(
        font.charmap().mappings().map(|(codepoint, _)| codepoint),
    ))
}
