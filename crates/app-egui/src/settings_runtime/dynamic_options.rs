use super::*;

/// Опрашивает один provider; выполняется в фоновом потоке refresh-а.
///
/// Ошибки провайдера и его отсутствие превращаются в error-snapshot —
/// семантика идентична прежнему синхронному refresh-у.
fn collect_provider_snapshot(
    provider_id: &OptionProviderId,
    request: SettingOptionsRequest,
    provider: Option<Arc<dyn SettingOptionProvider>>,
) -> SettingOptions {
    let current_value = request.current_value.clone();
    match provider {
        Some(provider) => match provider.options(request) {
            Ok(snapshot) => snapshot,
            Err(error) => options_snapshot_from_error(provider_id.clone(), current_value, error),
        },
        None => options_snapshot_from_error(
            provider_id.clone(),
            current_value,
            SettingOptionsError::ProviderUnavailable {
                provider_id: provider_id.clone(),
            },
        ),
    }
}

/// Создаёт production dynamic option providers.
#[cfg(not(test))]
pub(super) fn default_option_providers(
    audio_output_device_controller: audio::AudioOutputDeviceController,
) -> BTreeMap<OptionProviderId, Arc<dyn SettingOptionProvider>> {
    let provider = AudioOutputDeviceOptionProvider::new(audio_output_device_controller);
    let provider_id = provider.provider_id();
    let mut providers: BTreeMap<OptionProviderId, Arc<dyn SettingOptionProvider>> = BTreeMap::new();
    providers.insert(provider_id, Arc::new(provider));
    providers
}

/// В unit tests providers подставляются явно, чтобы unrelated tests не трогали CPAL/ALSA.
#[cfg(test)]
pub(super) fn default_option_providers(
    _audio_output_device_controller: audio::AudioOutputDeviceController,
) -> BTreeMap<OptionProviderId, Arc<dyn SettingOptionProvider>> {
    BTreeMap::new()
}

/// Dynamic options provider для `audio.output_device`.
#[cfg(not(test))]
struct AudioOutputDeviceOptionProvider {
    /// Audio owner API без CPAL types в settings runtime/UI.
    audio_output_device_controller: audio::AudioOutputDeviceController,
}

#[cfg(not(test))]
impl AudioOutputDeviceOptionProvider {
    /// Создаёт provider поверх shared audio output device controller-а.
    fn new(audio_output_device_controller: audio::AudioOutputDeviceController) -> Self {
        Self {
            audio_output_device_controller,
        }
    }
}

#[cfg(not(test))]
impl SettingOptionProvider for AudioOutputDeviceOptionProvider {
    fn provider_id(&self) -> OptionProviderId {
        OptionProviderId::from("audio.output_device")
    }

    fn options(
        &self,
        request: SettingOptionsRequest,
    ) -> Result<SettingOptions, SettingOptionsError> {
        let provider_id = self.provider_id();
        let mut options = vec![SettingOption::new(
            audio::DEFAULT_AUDIO_OUTPUT_DEVICE_ID,
            SettingText::new(
                "settings.audio.output_device.default",
                "Системное устройство",
            ),
        )];

        let devices = self
            .audio_output_device_controller
            .output_devices()
            .map_err(|error| SettingOptionsError::Failed {
                provider_id: provider_id.clone(),
                message: error.to_string(),
            })?;
        options.extend(devices.into_iter().map(audio_output_device_option));

        Ok(SettingOptions::ready(
            provider_id,
            options.clone(),
            current_option_value(request.current_value, &options),
        ))
    }
}

/// Преобразует neutral audio device snapshot в settings option.
#[cfg(not(test))]
fn audio_output_device_option(device: audio::AudioOutputDeviceInfo) -> SettingOption {
    let label = if device.is_system_default {
        format!("{} (текущее системное)", device.display_name)
    } else {
        device.display_name
    };

    SettingOption::new(
        device.stable_id,
        SettingText::new("settings.audio.output_device.detected", label),
    )
}

/// Создаёт snapshot с заданным status, сохраняя current select id.
fn options_snapshot_from_status(
    provider_id: OptionProviderId,
    status: SettingOptionsStatus,
    options: Vec<SettingOption>,
    current_value: Option<SettingValue>,
) -> SettingOptions {
    let current = current_option_value(current_value, &options);
    SettingOptions {
        provider_id,
        status,
        options,
        current,
    }
}

/// Создаёт error snapshot, который visual UI покажет как option-provider error.
fn options_snapshot_from_error(
    provider_id: OptionProviderId,
    current_value: Option<SettingValue>,
    error: SettingOptionsError,
) -> SettingOptions {
    options_snapshot_from_status(
        provider_id,
        SettingOptionsStatus::Unavailable {
            message: format!("Option-provider error: {error}"),
        },
        Vec::new(),
        current_value,
    )
}

/// Вычисляет current dynamic option state без замены unavailable id на default.
pub(super) fn current_option_value(
    current_value: Option<SettingValue>,
    options: &[SettingOption],
) -> SettingOptionCurrentValue {
    let Some(SettingValue::Select(current_id)) = current_value else {
        return SettingOptionCurrentValue::None;
    };

    if options.iter().any(|option| option.id == current_id) {
        SettingOptionCurrentValue::Available(current_id)
    } else {
        SettingOptionCurrentValue::UnavailableCurrent {
            label: unavailable_current_label(&current_id),
            id: current_id,
        }
    }
}

/// Формирует label для сохранённого, но сейчас недоступного dynamic option id.
fn unavailable_current_label(current_id: &SettingOptionId) -> SettingText {
    SettingText::new(
        "settings.dynamic_options.unavailable_current",
        format!("{} (сейчас недоступно)", current_id.as_str()),
    )
}

impl SettingsRuntime {
    /// Возвращает cached snapshot или neutral loading snapshot для dynamic select-а.
    pub(super) fn cached_options_for_descriptor(
        &self,
        descriptor: &settings_core::SettingDescriptor,
        field: &SettingsUiField,
    ) -> Option<SettingOptions> {
        let SettingEditor::Select(SelectDescriptor::Dynamic { provider_id }) = &descriptor.editor
        else {
            return None;
        };

        self.option_cache.get(provider_id).cloned().or_else(|| {
            Some(options_snapshot_from_status(
                provider_id.clone(),
                SettingOptionsStatus::Loading,
                Vec::new(),
                Some(field.draft_value.clone()),
            ))
        })
    }

    /// Обновляет все dynamic option providers, известные registry.
    pub(super) fn refresh_all_dynamic_options(&mut self) -> SettingsResult<()> {
        let provider_ids = self.dynamic_option_provider_ids();
        self.start_dynamic_options_refresh(provider_ids)
    }

    /// Запускает фоновый refresh одного dynamic option provider-а.
    pub(super) fn refresh_dynamic_options(
        &mut self,
        provider_id: OptionProviderId,
    ) -> SettingsResult<()> {
        self.start_dynamic_options_refresh(vec![provider_id])
    }

    /// Стартует фоновый поток опроса providers, не блокируя UI-поток.
    ///
    /// Requests собираются здесь (дёшево, нужен доступ к draft), а сам опрос
    /// устройств уходит в поток. Повторный старт заменяет pending refresh:
    /// старый receiver выбрасывается, результат устаревшего потока игнорируется.
    fn start_dynamic_options_refresh(
        &mut self,
        provider_ids: Vec<OptionProviderId>,
    ) -> SettingsResult<()> {
        if provider_ids.is_empty() {
            return Ok(());
        }

        let mut refresh_jobs = Vec::with_capacity(provider_ids.len());
        for provider_id in provider_ids {
            let request = self.option_request_for_provider(&provider_id)?;
            let provider = self.option_providers.get(&provider_id).cloned();
            refresh_jobs.push((provider_id, request, provider));
        }

        let (result_sender, result_receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("settings-options-refresh".to_string())
            .spawn(move || {
                let snapshots = refresh_jobs
                    .into_iter()
                    .map(|(provider_id, request, provider)| {
                        let snapshot = collect_provider_snapshot(&provider_id, request, provider);
                        (provider_id, snapshot)
                    })
                    .collect::<Vec<_>>();
                // Получатель мог исчезнуть (новый refresh заменил этот) — не ошибка.
                let _ = result_sender.send(snapshots);
            })
            .map_err(|error| {
                settings_core::SettingsError::access_failed(format!(
                    "не удалось запустить фоновый refresh dynamic options: {error}"
                ))
            })?;

        self.pending_options_refresh = Some(result_receiver);
        Ok(())
    }

    /// `true`, пока фоновый refresh dynamic options ещё не доставил результат.
    /// Shell использует это для пробуждения idle loop под background polling.
    #[must_use]
    pub(crate) fn has_pending_options_refresh(&self) -> bool {
        self.pending_options_refresh.is_some()
    }

    /// Подбирает результат фонового refresh-а, если он готов.
    ///
    /// Возвращает `true`, когда cache обновился и model была инвалидирована.
    /// Вызывается на каждом кадре перед сборкой `ui_model`; `try_recv` дешёвый.
    pub(crate) fn poll_dynamic_options_refresh(&mut self) -> bool {
        let Some(result_receiver) = self.pending_options_refresh.as_ref() else {
            return false;
        };
        match result_receiver.try_recv() {
            Ok(snapshots) => {
                self.pending_options_refresh = None;
                self.apply_dynamic_options_snapshots(snapshots);
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => {
                // Поток умер, не отправив результат (panic). Pending снимаем,
                // чтобы не будить idle loop вечно; старый cache остаётся валидным.
                self.pending_options_refresh = None;
                tracing::warn!(
                    "фоновый refresh dynamic options завершился без результата (panic потока?)"
                );
                false
            }
        }
    }

    /// Кладёт готовые snapshots провайдеров в cache и инвалидирует visual model.
    fn apply_dynamic_options_snapshots(
        &mut self,
        snapshots: Vec<(OptionProviderId, SettingOptions)>,
    ) {
        for (provider_id, snapshot) in snapshots {
            self.option_cache.insert(provider_id, snapshot);
        }
        self.invalidate_ui_model();
    }

    /// Блокирующее ожидание фонового refresh-а для детерминированных тестов.
    #[cfg(test)]
    pub(crate) fn wait_for_options_refresh_for_test(&mut self) {
        if let Some(result_receiver) = self.pending_options_refresh.take() {
            let snapshots = result_receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("фоновый refresh dynamic options должен завершиться в тесте");
            self.apply_dynamic_options_snapshots(snapshots);
        }
    }

    /// Собирает request для provider-а из текущего draft value.
    fn option_request_for_provider(
        &self,
        provider_id: &OptionProviderId,
    ) -> SettingsResult<SettingOptionsRequest> {
        for descriptor in self.registry().descriptors() {
            let SettingEditor::Select(SelectDescriptor::Dynamic {
                provider_id: descriptor_provider_id,
            }) = &descriptor.editor
            else {
                continue;
            };
            if descriptor_provider_id != provider_id {
                continue;
            }

            let current_value = self
                .registry()
                .get_value(self.controller.draft(), &descriptor.id)?;
            return Ok(SettingOptionsRequest::new(
                descriptor.id.clone(),
                Some(current_value),
            ));
        }

        Ok(SettingOptionsRequest::new(provider_id.as_str(), None))
    }

    /// Возвращает provider ids из registry без дублей.
    fn dynamic_option_provider_ids(&self) -> Vec<OptionProviderId> {
        self.registry()
            .descriptors()
            .filter_map(|descriptor| match &descriptor.editor {
                SettingEditor::Select(SelectDescriptor::Dynamic { provider_id }) => {
                    Some(provider_id.clone())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}
