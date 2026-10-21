use super::*;

/// Эталон СТАРОГО алгоритма (`Vec` + `drain(..)` с начала) для сравнения выхода.
#[derive(Default)]
struct ReferenceHistory {
    samples: Vec<f32>,
    spec: Option<audio_core::AudioOutputSpec>,
}

impl ReferenceHistory {
    fn record(&mut self, new_samples: &[f32], output_spec: audio_core::AudioOutputSpec) {
        let channels = output_spec.channels();
        if output_spec.sample_rate == 0 || channels == 0 || new_samples.is_empty() {
            return;
        }
        if self.spec != Some(output_spec) {
            self.samples.clear();
            self.spec = Some(output_spec);
        }
        self.samples.extend_from_slice(new_samples);
        // 600 ms бюджета, как в production-константе.
        let frames = (u64::from(output_spec.sample_rate) * 600).div_ceil(1000);
        let max_samples = (frames * u64::from(channels)) as usize;
        let overflow = self.samples.len().saturating_sub(max_samples);
        if overflow > 0 {
            let aligned = overflow.next_multiple_of(channels as usize);
            let end = aligned.min(self.samples.len());
            self.samples.drain(..end);
        }
    }

    fn take(&mut self, output_spec: audio_core::AudioOutputSpec) -> Vec<f32> {
        let history = std::mem::take(&mut self.samples);
        let matches = self.spec == Some(output_spec);
        self.spec = None;
        if matches { history } else { Vec::new() }
    }
}

/// Детерминированные пакеты разного размера (включая не кратные числу каналов).
fn packet(counter: &mut u32, len: usize) -> Vec<f32> {
    (0..len)
        .map(|_| {
            *counter += 1;
            *counter as f32
        })
        .collect()
}

#[test]
fn passthrough_history_ring_matches_reference_algorithm_on_mixed_sequence() {
    let stereo = audio_output_spec_for_tests(100, 2); // бюджет 120 сэмплов
    let tri = audio_output_spec_for_tests(100, 3); // бюджет 180 сэмплов
    let mut pipeline = PlaybackPipeline::default();
    let mut reference = ReferenceHistory::default();
    let mut counter = 0_u32;

    // (spec, длина пакета): переполнение, нечётная длина (сдвиг выравнивания),
    // пакет больше бюджета, смена spec, пустой пакет.
    let steps = [
        (stereo, 30),
        (stereo, 45),
        (stereo, 7),
        (stereo, 100),
        (stereo, 0),
        (stereo, 301),
        (tri, 50),
        (tri, 200),
        (tri, 11),
        (stereo, 9),
        (stereo, 64),
    ];
    for (index, (spec, len)) in steps.into_iter().enumerate() {
        let chunk = packet(&mut counter, len);
        pipeline.record_passthrough_audio_history(&chunk, spec);
        reference.record(&chunk, spec);
        assert_eq!(
            pipeline.passthrough_audio_history_output_spec(),
            reference.spec,
            "spec на шаге {index}"
        );
        // Периодически забираем историю: take обязан совпасть побайтно, а повторный — быть пустым.
        if index % 4 == 3 {
            let taken = pipeline.take_passthrough_audio_history_for_priming(spec);
            assert_eq!(taken, reference.take(spec), "take на шаге {index}");
            assert!(!taken.is_empty(), "после записи take не должен быть пустым");
            assert!(
                pipeline
                    .take_passthrough_audio_history_for_priming(spec)
                    .is_empty()
            );
            assert_eq!(pipeline.passthrough_audio_history_output_spec(), None);
        }
    }

    let final_spec = stereo;
    assert_eq!(
        pipeline.take_passthrough_audio_history_for_priming(final_spec),
        reference.take(final_spec)
    );
}

#[test]
fn passthrough_history_clear_and_foreign_spec_drop_everything() {
    let stereo = audio_output_spec_for_tests(100, 2);
    let mono = audio_output_spec_for_tests(100, 1);
    let mut pipeline = PlaybackPipeline::default();

    pipeline.record_passthrough_audio_history(&[1.0, 2.0, 3.0, 4.0], stereo);
    pipeline.clear_passthrough_audio_history();
    assert_eq!(pipeline.passthrough_audio_history_output_spec(), None);
    assert!(
        pipeline
            .take_passthrough_audio_history_for_priming(stereo)
            .is_empty()
    );

    // Чужой spec: take пуст, а история при этом всё равно потреблена.
    pipeline.record_passthrough_audio_history(&[1.0, 2.0], stereo);
    assert!(
        pipeline
            .take_passthrough_audio_history_for_priming(mono)
            .is_empty()
    );
    assert!(
        pipeline
            .take_passthrough_audio_history_for_priming(stereo)
            .is_empty()
    );
}

#[test]
fn passthrough_history_does_not_reallocate_or_exceed_bound_after_warmup() {
    // 48 kHz stereo: бюджет 600 ms = 57_600 сэмплов; пакеты по 1920 сэмплов (20 ms).
    let spec = audio_output_spec_for_tests(48_000, 2);
    let max_samples = 57_600_usize;
    let mut pipeline = PlaybackPipeline::default();
    let mut counter = 0_u32;

    // Прогрев: заполняем историю с запасом, ёмкость после этого обязана стать стабильной.
    for _ in 0..40 {
        let chunk = packet(&mut counter, 1920);
        pipeline.record_passthrough_audio_history(&chunk, spec);
    }
    let warmed_capacity = pipeline.passthrough_audio_history.capacity_for_tests();

    // На старом `Vec` + drain(..) каждый пакет двигал всю историю; здесь же 500 пакетов
    // с постоянным переполнением не меняют ёмкость кольца (нет реаллокаций/роста памяти).
    for _ in 0..500 {
        let chunk = packet(&mut counter, 1920);
        pipeline.record_passthrough_audio_history(&chunk, spec);
        assert_eq!(
            pipeline.passthrough_audio_history.capacity_for_tests(),
            warmed_capacity
        );
    }
    // Память не больше, чем у старой реализации (бюджет + один пакет с запасом роста Vec).
    assert!(warmed_capacity <= (max_samples + 1920) * 2);

    let history = pipeline.take_passthrough_audio_history_for_priming(spec);
    assert_eq!(history.len(), max_samples);
    assert_eq!(history.last().copied(), Some(counter as f32));
}
