//! Что store знает об общем config-файле: последний виденный документ диска и изменения,
//! которые ещё не попали в файл, — и правило их дописывания.
//!
//! Если запись не удалась (файл занят, битый, новее…) или запрещена до тех пор, пока файл
//! снова не станет нормальным (восстановимый режим «только на сеанс»), изменение уже
//! применено и живёт в памяти (решение 135). Store запоминает id таких настроек и при
//! следующей успешной записи дописывает их, беря значение из **текущего** документа в
//! памяти: если настройку с тех пор меняли ещё раз, запишется последнее значение.
//!
//! Правило «не затереть соседа». Для каждого несохранённого id store помнит *базу* —
//! значение этого id на диске, каким его **видела эта копия** в момент последнего
//! несохранённого изменения. «Видела» — это последний документ диска, о котором копия
//! точно знает ([`SharedFileKnowledge::note_disk_config_seen`]):
//! - документ, прочитанный при старте (если файл был прочитан или создан);
//! - документ, который копия сама только что успешно записала;
//! - документ, который «общая доска» прочитала, следя за файлом (`SharedConfigRead::Parsed`).
//!
//! Почему не committed в памяти (`before` изменения): committed не обязан совпадать с
//! диском. Сосед записал громкость 0.3, эта копия её не принимает (решение 137) и держит
//! 0.8; пользователь ставит 0.7, запись отложена. С базой 0.8 дописывание увидело бы на
//! диске 0.3 ≠ 0.8 и решило бы, что сосед писал позже, — и выбросило бы 0.7, хотя сосед
//! писал раньше. С базой «видели 0.3» диск 0.3 = база — после нас ключ никто не трогал.
//!
//! При дописывании под замком:
//! - значение на диске уже равно значению в памяти — писать нечего (сосед записал то же
//!   самое, или доска приняла это значение от соседа);
//! - значение на диске равно базе — после нашего изменения ключ никто не трогал,
//!   записываем значение из памяти;
//! - значение на диске другое — после нашего изменения ключ записала другая копия. Она
//!   записала его позже, а «один ключ у двоих — побеждает последний» (решение 135),
//!   поэтому значение с диска остаётся, а наше несохранённое забывается. Подтянуть его в
//!   память — дело слежения за файлом.
//!
//! База неизвестна ([`DiskBaseline::NeverSeen`]): копия не видела ни одного нормального
//! документа (файл при старте был битым, отсутствовал под занятым замком и т. п.). Тогда
//! первый нормальный документ, который копия увидит, становится базой: это починка или
//! первое создание файла, а не чей-то более поздний выбор этого ключа, и выбор
//! пользователя в этом окне не должен им стираться. Если первым «увиденным» окажется
//! документ под замком при самой записи — значение из памяти записывается.

use std::collections::BTreeMap;

use fastiplayer_config::AppConfig;
use settings_core::{SettingId, SettingValue, SettingsDiff, SettingsRegistry, SettingsResult};

/// Значение id на диске, от которого отталкивалось несохранённое изменение.
#[derive(Debug, Clone, PartialEq)]
enum DiskBaseline {
    /// Значение из последнего документа диска, который видела эта копия.
    Seen(SettingValue),
    /// Копия ещё не видела ни одного нормального документа (см. модуль).
    NeverSeen,
}

/// Знание store об общем config-файле.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SharedFileKnowledge {
    /// Последний документ диска, который видела эта копия (`None` — не видела ни одного).
    seen_on_disk: Option<Box<AppConfig>>,
    /// id несохранённых настроек → база.
    disk_baselines: BTreeMap<SettingId, DiskBaseline>,
}

impl SharedFileKnowledge {
    /// Знание копии, которая при старте видела `seen_on_disk` (или не видела ничего).
    pub(crate) fn starting_from(seen_on_disk: Option<&AppConfig>) -> Self {
        Self {
            seen_on_disk: seen_on_disk.map(|document| Box::new(document.clone())),
            disk_baselines: BTreeMap::new(),
        }
    }

    /// Есть ли изменения, которые ещё не попали в файл.
    pub(crate) fn has_unsaved(&self) -> bool {
        !self.disk_baselines.is_empty()
    }

    /// Id несохранённых настроек в стабильном порядке.
    pub(crate) fn unsaved_setting_ids(&self) -> Vec<SettingId> {
        self.disk_baselines.keys().cloned().collect()
    }

    /// Копия увидела нормальный документ диска (своя запись или чтение доски).
    ///
    /// Базы, которые были неизвестны, получают значения этого документа (см. модуль);
    /// уже известные базы не меняются — они описывают момент изменения пользователя.
    pub(crate) fn note_disk_config_seen(
        &mut self,
        registry: &SettingsRegistry<AppConfig>,
        disk_document: &AppConfig,
    ) -> SettingsResult<()> {
        let mut resolved = Vec::new();
        for (setting_id, baseline) in &self.disk_baselines {
            if *baseline == DiskBaseline::NeverSeen {
                resolved.push((
                    setting_id.clone(),
                    registry.get_value(disk_document, setting_id)?,
                ));
            }
        }
        // Мутации только после того, как все значения прочитаны: ошибка реестра ничего
        // не меняет наполовину.
        for (setting_id, disk_value) in resolved {
            self.disk_baselines
                .insert(setting_id, DiskBaseline::Seen(disk_value));
        }
        self.seen_on_disk = Some(Box::new(disk_document.clone()));
        Ok(())
    }

    /// Запоминает изменения, которые сейчас не попали в файл.
    ///
    /// База — значение id в последнем виденном документе диска **на момент этого
    /// изменения**: если копия уже видела, что сосед записал ключ, новое изменение
    /// пользователя сделано позже соседского и при дописывании его не уступит.
    pub(crate) fn remember_unwritten(
        &mut self,
        registry: &SettingsRegistry<AppConfig>,
        changed_settings: &SettingsDiff,
    ) -> SettingsResult<()> {
        let mut baselines = Vec::with_capacity(changed_settings.changes().len());
        for change in changed_settings.changes() {
            let baseline = match &self.seen_on_disk {
                Some(seen) => DiskBaseline::Seen(registry.get_value(seen, &change.id)?),
                None => DiskBaseline::NeverSeen,
            };
            baselines.push((change.id.clone(), baseline));
        }
        self.disk_baselines.extend(baselines);
        Ok(())
    }

    /// Файл записан: всё несохранённое записано или уступило соседу, а записанный
    /// документ — теперь последнее, что эта копия видела на диске.
    pub(crate) fn note_written(&mut self, written: Box<AppConfig>) {
        self.disk_baselines.clear();
        self.seen_on_disk = Some(written);
    }
}

/// Что стало с несохранёнными id при одной записи.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct UnsavedOverlay {
    /// Дописаны значениями из памяти.
    pub(crate) written: Vec<SettingId>,
    /// На диске уже было то же значение, что в памяти.
    pub(crate) already_on_disk: Vec<SettingId>,
    /// После нашего изменения ключ записала другая копия — оставлено её значение.
    pub(crate) kept_from_disk: Vec<SettingId>,
}

/// Вписывает в документ с диска новые изменения транзакции и дописывает несохранённые.
///
/// Новые изменения (`changed_settings`) пишутся значением `after` всегда: пользователь
/// сделал их только что, значит его запись — последняя. Несохранённые id, которые не
/// входят в новые изменения, проходят правило «не затереть соседа» (см. модуль).
pub(crate) fn overlay_changes(
    registry: &SettingsRegistry<AppConfig>,
    disk_config: &mut AppConfig,
    changed_settings: &SettingsDiff,
    knowledge: &SharedFileKnowledge,
    memory_config: &AppConfig,
) -> SettingsResult<UnsavedOverlay> {
    for change in changed_settings.changes() {
        registry.set_value(disk_config, &change.id, change.after.clone())?;
    }

    let mut overlay = UnsavedOverlay::default();
    for (setting_id, disk_baseline) in &knowledge.disk_baselines {
        let changed_now = changed_settings
            .changes()
            .iter()
            .any(|change| change.id == *setting_id);
        if changed_now {
            overlay.written.push(setting_id.clone());
            continue;
        }
        let disk_value = registry.get_value(disk_config, setting_id)?;
        let memory_value = registry.get_value(memory_config, setting_id)?;
        let untouched_since_our_change = match disk_baseline {
            DiskBaseline::Seen(baseline_value) => disk_value == *baseline_value,
            // Первый нормальный документ, который видит копия, — база (см. модуль).
            DiskBaseline::NeverSeen => true,
        };
        if disk_value == memory_value {
            overlay.already_on_disk.push(setting_id.clone());
        } else if untouched_since_our_change {
            registry.set_value(disk_config, setting_id, memory_value)?;
            overlay.written.push(setting_id.clone());
        } else {
            overlay.kept_from_disk.push(setting_id.clone());
        }
    }
    Ok(overlay)
}
