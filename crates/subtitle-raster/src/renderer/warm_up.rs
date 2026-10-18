//! Фоновый прогрев шрифтов: подготовить системные лица, которые понадобятся тексту дорожки,
//! ДО того, как на экране появится первая реплика.
//!
//! Первая реплика с CJK-символами платит за чтение файла шрифта с диска (`.ttc` бывает
//! десятки МБ), разбор таблиц и ранжирование системных кандидатов. Прогрев делает ту же работу
//! заранее, пока рисовальщик простаивает. Он трогает только то, что принадлежит шрифтам:
//! кэш поиска глифов библиотеки и кэш лиц. Кэши реплик и глифов, состояние кадра и история
//! раскладки не затрагиваются — прогрев невидим для результата рисования.
//!
//! Память и диск не тратятся впустую (решение владельца):
//! - прогреваются только лица, которые реально нужны символам образца;
//! - встроенные лица пропускаются (они и так в бинарнике и подготавливаются мгновенно);
//! - число подготовленных лиц не выходит за половину лимита `shaper_faces`, чтобы прогрев не
//!   вытеснил лица текущей реплики и не был срезан `trim_to` в начале кадра.

use crate::fonts::{
    FaceOrigin, FontRequest, FontStyle, FontWeightClass, GlyphLookup, TextLanguagePreference,
};

use super::SubtitleRasterizer;

/// Итог прогрева шрифтов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontWarmUp {
    /// Подготавливать нечего: образец пуст, покрыт встроенными шрифтами, нужные лица уже
    /// подготовлены, или ни один шрифт не содержит его символов.
    NothingToWarm,
    /// Подготовлены новые лица.
    Warmed {
        /// Сколько лиц загружено этим вызовом.
        faces_loaded: usize,
    },
    /// Прогрев остановлен по лимиту числа лиц; часть нужных лиц осталась неподготовленной.
    StoppedAtFaceBudget {
        /// Сколько лиц успели загрузить до остановки.
        faces_loaded: usize,
    },
}

impl SubtitleRasterizer {
    /// Заранее подготавливает системные (и вложенные) лица, нужные символам `sample`.
    ///
    /// Лицо для каждого символа выбирается тем же путём, что и при рисовании реплики без
    /// собственного шрифта (основное лицо по умолчанию + fallback по глифу с учётом
    /// `language`), поэтому результат поиска ложится в тот же кэш и первая настоящая реплика
    /// находит и лицо, и глиф готовыми. Вызывать только когда рисовальщик простаивает: загрузка
    /// лица читает файл с диска. Ошибка загрузки лица не фатальна: лицо помечается
    /// непригодным, как при рисовании, и прогрев идёт дальше.
    pub fn warm_up_fonts(&mut self, sample: &str, language: TextLanguagePreference) -> FontWarmUp {
        let default_face = self
            .fonts
            .resolve_family(&FontRequest {
                family: None,
                weight: FontWeightClass::Regular,
                style: FontStyle::Normal,
            })
            .face;
        let face_budget = self.limits.cache.shaper_faces / 2;
        let mut faces_loaded = 0_usize;
        let mut seen_chars: Vec<char> = Vec::new();
        for ch in sample.chars() {
            // Управляющие символы шрифт не ищут; повтор символа ничего нового не даст.
            if u32::from(ch) < 0x20 || seen_chars.contains(&ch) {
                continue;
            }
            seen_chars.push(ch);
            let GlyphLookup::Found { face, .. } =
                self.fonts.glyph_for_char(default_face, ch, language)
            else {
                continue;
            };
            let is_builtin = self
                .fonts
                .describe_face(face)
                .is_none_or(|description| description.origin == FaceOrigin::Embedded);
            if is_builtin || self.faces.get(face).is_ok() {
                continue;
            }
            if self.faces.statistics().cached_faces >= face_budget {
                return FontWarmUp::StoppedAtFaceBudget { faces_loaded };
            }
            match self.faces.ensure_loaded(&self.fonts, face) {
                Ok(()) => faces_loaded += 1,
                Err(error) => {
                    tracing::debug!(%error, "font warm-up skipped a face that failed to load");
                    self.fonts.mark_face_unusable(face);
                }
            }
        }
        if faces_loaded == 0 {
            FontWarmUp::NothingToWarm
        } else {
            FontWarmUp::Warmed { faces_loaded }
        }
    }
}
