use super::*;

/// Один подготовленный batch provider requests для background refresh.
pub(super) type DynamicOptionsRefreshWork = Vec<(
    OptionProviderId,
    SettingOptionsRequest,
    Option<Arc<dyn SettingOptionProvider>>,
)>;

/// Неблокирующий terminal результат одного refresh job-а.
enum DynamicOptionsRefreshCompletion {
    /// Authoritative latest snapshots можно применить к cache.
    Snapshots(Vec<(OptionProviderId, SettingOptions)>),

    /// Superseded/shutdown job завершён и joined без apply authority.
    Cancelled,

    /// Worker panic или потеря exactly-once terminal mailbox.
    Failed,
}

/// Один bounded-owned refresh thread и его exactly-once mailbox.
pub(super) struct DynamicOptionsRefreshJob {
    /// Completion не применяется до exact worker exit и join.
    result_receiver:
        crate::app_wake::OwnerMailboxReceiver<(), Vec<(OptionProviderId, SettingOptions)>>,

    /// Join authority никогда не выбрасывается при replacement.
    join_handle: Option<std::thread::JoinHandle<()>>,

    /// Mailbox result ждёт завершения thread-а внутри owner-а.
    pending_result: Option<Vec<(OptionProviderId, SettingOptions)>>,

    /// Cooperative cancellation делает superseded result неавторитетным.
    cancellation_requested: Arc<std::sync::atomic::AtomicBool>,
}

impl DynamicOptionsRefreshJob {
    /// Запускает один refresh batch с owned handle и secret-free error mapping.
    fn spawn(
        refresh_work: DynamicOptionsRefreshWork,
        wake_port: AppWakePort,
    ) -> SettingsResult<Self> {
        let (result_publisher, result_receiver) = crate::app_wake::owner_mailbox(wake_port);
        let cancellation_requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_cancellation_requested = Arc::clone(&cancellation_requested);
        let join_handle = std::thread::Builder::new()
            .name("settings-options-refresh".to_string())
            .spawn(move || {
                let snapshots = refresh_work
                    .into_iter()
                    .take_while(|_| {
                        !worker_cancellation_requested.load(std::sync::atomic::Ordering::Acquire)
                    })
                    .map(|(provider_id, request, provider)| {
                        let snapshot = collect_provider_snapshot(&provider_id, request, provider);
                        (provider_id, snapshot)
                    })
                    .collect::<Vec<_>>();
                if worker_cancellation_requested.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                match result_publisher.publish_completion(snapshots) {
                    Ok(crate::app_wake::WakeDelivery::EventLoopClosed) => {
                        tracing::debug!(
                            "Event loop закрыт; settings terminal оставлен без wake retry"
                        );
                    }
                    Ok(
                        crate::app_wake::WakeDelivery::Armed
                        | crate::app_wake::WakeDelivery::Coalesced,
                    ) => {}
                    Err(crate::app_wake::CompletionPublishError::AlreadyPublished) => {
                        tracing::warn!(
                            "dynamic options refresh попытался опубликовать второй terminal result"
                        );
                    }
                }
            })
            .map_err(|error| {
                settings_core::SettingsError::access_failed(format!(
                    "не удалось запустить фоновый refresh dynamic options: {error}"
                ))
            })?;

        Ok(Self {
            result_receiver,
            join_handle: Some(join_handle),
            pending_result: None,
            cancellation_requested,
        })
    }

    /// Делает job stale и запрещает публикацию позднего результата.
    fn cancel(&self) {
        self.cancellation_requested
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Reap-ит только finished handle и возвращает authoritative snapshots.
    fn take_finished_result(&mut self) -> Option<DynamicOptionsRefreshCompletion> {
        let drain = self.result_receiver.drain();
        if drain.completion.is_some() {
            self.pending_result = drain.completion;
        }

        match crate::process_shutdown::join_finished_thread(&mut self.join_handle) {
            crate::process_shutdown::FinishedThreadJoin::StillRunning => None,
            crate::process_shutdown::FinishedThreadJoin::Panicked => {
                Some(DynamicOptionsRefreshCompletion::Failed)
            }
            crate::process_shutdown::FinishedThreadJoin::Joined
            | crate::process_shutdown::FinishedThreadJoin::AlreadyJoined => {
                if self
                    .cancellation_requested
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    self.pending_result = None;
                    Some(DynamicOptionsRefreshCompletion::Cancelled)
                } else if let Some(snapshots) = self.pending_result.take() {
                    Some(DynamicOptionsRefreshCompletion::Snapshots(snapshots))
                } else if drain.producer_disconnected_without_completion {
                    Some(DynamicOptionsRefreshCompletion::Failed)
                } else {
                    None
                }
            }
        }
    }

    /// Cooperative-cancel-ит job и bounded-join-ит его exact handle.
    fn shutdown_until(
        &mut self,
        deadline: crate::process_shutdown::ShutdownDeadline,
    ) -> crate::process_shutdown::ProcessOwnerShutdownOutcome {
        self.cancel();
        match crate::process_shutdown::join_thread_until(&mut self.join_handle, deadline) {
            crate::process_shutdown::FinishedThreadJoin::AlreadyJoined
            | crate::process_shutdown::FinishedThreadJoin::Joined => {
                crate::process_shutdown::ProcessOwnerShutdownOutcome::Completed
            }
            crate::process_shutdown::FinishedThreadJoin::StillRunning => {
                crate::process_shutdown::ProcessOwnerShutdownOutcome::TimedOut {
                    pending_threads: 1,
                }
            }
            crate::process_shutdown::FinishedThreadJoin::Panicked => {
                crate::process_shutdown::ProcessOwnerShutdownOutcome::ThreadPanicked {
                    panicked_threads: 1,
                    pending_threads: 0,
                }
            }
        }
    }
}

impl Drop for DynamicOptionsRefreshJob {
    fn drop(&mut self) {
        self.cancel();
        let Some(join_handle) = self.join_handle.take() else {
            return;
        };
        if join_handle.join().is_err() {
            tracing::warn!("dynamic options refresh panic обнаружен во время fail-safe Drop");
        }
    }
}

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
        if self.dynamic_options_shutdown_started {
            return Err(settings_core::SettingsError::access_failed(
                "dynamic options shutdown уже начат; новый refresh запрещён",
            ));
        }

        let mut refresh_work = Vec::with_capacity(provider_ids.len());
        for provider_id in provider_ids {
            let request = self.option_request_for_provider(&provider_id)?;
            let provider = self.option_providers.get(&provider_id).cloned();
            refresh_work.push((provider_id, request, provider));
        }

        self.reap_retired_options_refresh();
        if let Some(active_job) = self.active_options_refresh.take() {
            active_job.cancel();
            if self.retired_options_refresh.is_none() {
                self.retired_options_refresh = Some(active_job);
                self.active_options_refresh = Some(DynamicOptionsRefreshJob::spawn(
                    refresh_work,
                    self.dynamic_options_wake_port.clone(),
                )?);
            } else {
                self.active_options_refresh = Some(active_job);
                self.pending_latest_options_refresh = Some(refresh_work);
            }
            return Ok(());
        }

        self.active_options_refresh = Some(DynamicOptionsRefreshJob::spawn(
            refresh_work,
            self.dynamic_options_wake_port.clone(),
        )?);
        Ok(())
    }

    /// `true`, пока фоновый refresh dynamic options ещё не доставил результат.
    /// Shell использует это для пробуждения idle loop под background polling.
    #[must_use]
    pub(crate) fn has_pending_options_refresh(&self) -> bool {
        self.active_options_refresh.is_some()
            || self.retired_options_refresh.is_some()
            || self.pending_latest_options_refresh.is_some()
    }

    /// Подбирает результат фонового refresh-а, если он готов.
    ///
    /// Возвращает `true`, когда cache обновился и model была инвалидирована.
    /// Вызывается на каждом кадре перед сборкой `ui_model`; `try_recv` дешёвый.
    pub(crate) fn poll_dynamic_options_refresh(&mut self) -> bool {
        self.reap_retired_options_refresh();

        let Some(active_job) = self.active_options_refresh.as_mut() else {
            if self.start_pending_latest_options_refresh().is_err() {
                tracing::warn!("не удалось запустить latest dynamic options refresh");
            }
            self.dynamic_options_wake_port
                .acknowledge_abandoned_mailbox();
            return false;
        };
        let Some(result) = active_job.take_finished_result() else {
            return false;
        };
        self.active_options_refresh = None;

        let had_visible_mutation = match result {
            DynamicOptionsRefreshCompletion::Snapshots(snapshots)
                if self.pending_latest_options_refresh.is_none() =>
            {
                self.apply_dynamic_options_snapshots(snapshots);
                true
            }
            DynamicOptionsRefreshCompletion::Snapshots(_)
            | DynamicOptionsRefreshCompletion::Cancelled => false,
            DynamicOptionsRefreshCompletion::Failed => {
                tracing::warn!("фоновый refresh dynamic options завершился без terminal result");
                false
            }
        };
        if self.start_pending_latest_options_refresh().is_err() {
            tracing::warn!("не удалось запустить latest dynamic options refresh");
        }
        had_visible_mutation
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
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.has_pending_options_refresh() {
            let _visible_mutation = self.poll_dynamic_options_refresh();
            assert!(
                Instant::now() < deadline,
                "фоновый refresh dynamic options должен завершиться в тесте"
            );
            std::thread::yield_now();
        }
    }

    /// Reap-ит единственный retired job без применения stale snapshots.
    fn reap_retired_options_refresh(&mut self) {
        let Some(retired_job) = self.retired_options_refresh.as_mut() else {
            return;
        };
        if retired_job.take_finished_result().is_some() {
            self.retired_options_refresh = None;
        }
    }

    /// Запускает capacity-one latest work после освобождения active slot-а.
    fn start_pending_latest_options_refresh(&mut self) -> SettingsResult<()> {
        let Some(refresh_work) = self.pending_latest_options_refresh.take() else {
            return Ok(());
        };
        if self.active_options_refresh.is_some() {
            self.pending_latest_options_refresh = Some(refresh_work);
            return Ok(());
        }
        self.active_options_refresh = Some(DynamicOptionsRefreshJob::spawn(
            refresh_work,
            self.dynamic_options_wake_port.clone(),
        )?);
        Ok(())
    }

    /// Закрывает refresh admission и bounded-завершает active/retired threads.
    pub(crate) fn shutdown_dynamic_options_until(
        &mut self,
        deadline: crate::process_shutdown::ShutdownDeadline,
    ) -> crate::process_shutdown::ProcessOwnerShutdownOutcome {
        use crate::process_shutdown::ProcessOwnerShutdownOutcome;

        if self.dynamic_options_shutdown_completed {
            return ProcessOwnerShutdownOutcome::AlreadyCompleted;
        }
        self.dynamic_options_shutdown_started = true;
        self.pending_latest_options_refresh = None;

        // Сначала закрываем apply authority у обоих bounded slots; только затем
        // первый join может начать расходовать общий deadline.
        if let Some(active_job) = self.active_options_refresh.as_ref() {
            active_job.cancel();
        }
        if let Some(retired_job) = self.retired_options_refresh.as_ref() {
            retired_job.cancel();
        }

        let mut panicked_threads = 0;
        let mut pending_threads = 0;
        if let Some(active_job) = self.active_options_refresh.as_mut() {
            accumulate_dynamic_options_shutdown(
                active_job.shutdown_until(deadline),
                &mut panicked_threads,
                &mut pending_threads,
            );
            if active_job.join_handle.is_none() {
                self.active_options_refresh = None;
            }
        }
        if let Some(retired_job) = self.retired_options_refresh.as_mut() {
            accumulate_dynamic_options_shutdown(
                retired_job.shutdown_until(deadline),
                &mut panicked_threads,
                &mut pending_threads,
            );
            if retired_job.join_handle.is_none() {
                self.retired_options_refresh = None;
            }
        }

        if pending_threads > 0 {
            if panicked_threads > 0 {
                return ProcessOwnerShutdownOutcome::ThreadPanicked {
                    panicked_threads,
                    pending_threads,
                };
            }
            return ProcessOwnerShutdownOutcome::TimedOut { pending_threads };
        }

        self.dynamic_options_shutdown_completed = true;
        if panicked_threads > 0 {
            ProcessOwnerShutdownOutcome::ThreadPanicked {
                panicked_threads,
                pending_threads: 0,
            }
        } else {
            ProcessOwnerShutdownOutcome::Completed
        }
    }

    /// Возвращает bounded число owned handles для focused tests.
    #[cfg(test)]
    pub(crate) fn dynamic_options_owned_thread_count(&self) -> usize {
        usize::from(self.active_options_refresh.is_some())
            + usize::from(self.retired_options_refresh.is_some())
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

/// Агрегирует shutdown outcome двух bounded settings slots.
fn accumulate_dynamic_options_shutdown(
    outcome: crate::process_shutdown::ProcessOwnerShutdownOutcome,
    panicked_threads: &mut usize,
    pending_threads: &mut usize,
) {
    use crate::process_shutdown::ProcessOwnerShutdownOutcome;

    match outcome {
        ProcessOwnerShutdownOutcome::Completed | ProcessOwnerShutdownOutcome::AlreadyCompleted => {}
        ProcessOwnerShutdownOutcome::TimedOut {
            pending_threads: pending,
        } => *pending_threads += pending,
        ProcessOwnerShutdownOutcome::ThreadPanicked {
            panicked_threads: panicked,
            pending_threads: pending,
        } => {
            *panicked_threads += panicked;
            *pending_threads += pending;
        }
    }
}
