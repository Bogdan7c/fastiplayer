//! Общие exact fixtures, typed requests и structural mutation helpers.

use std::num::NonZeroU32;
use std::ops::Range;

use super::super::error::FragmentInspectionError;
use super::super::inspect::inspect_media_fragment;
use super::super::limits::FragmentInspectionLimits;
use super::super::model::{
    FragmentBaseDecodeTime, FragmentCompositionOffsetSemantics, FragmentInspectionRequest,
    FragmentRapRequirement, FragmentSampleDefaults, FragmentTrackExpectation, FragmentTrackId,
    NormalizedFragmentPlan,
};

/// Exact Unified Streaming video rendition, первый fragment.
pub(super) const VIDEO_HIGH_FIRST: &[u8] =
    include_bytes!("../../../fixtures/smooth-piff/video-1501000-0.bin");
/// Exact Unified Streaming video rendition, второй fragment.
pub(super) const VIDEO_HIGH_SECOND: &[u8] =
    include_bytes!("../../../fixtures/smooth-piff/video-1501000-40000000.bin");
/// Exact Unified Streaming low video rendition.
pub(super) const VIDEO_LOW_FIRST: &[u8] =
    include_bytes!("../../../fixtures/smooth-piff/video-401000-0.bin");
/// Exact Unified Streaming audio rendition, первый fragment.
pub(super) const AUDIO_FIRST: &[u8] =
    include_bytes!("../../../fixtures/smooth-piff/audio-64008-0.bin");
/// Exact Unified Streaming audio rendition, второй fragment.
pub(super) const AUDIO_SECOND: &[u8] =
    include_bytes!("../../../fixtures/smooth-piff/audio-64008-39680000.bin");
/// Captured manifest используется только как provenance/timing evidence, не parser fixture.
pub(super) const MANIFEST: &[u8] =
    include_bytes!("../../../fixtures/smooth-piff/tears-of-steel.ismc");

/// Собирает explicit generous budgets без production defaults.
pub(super) fn limits() -> FragmentInspectionLimits {
    FragmentInspectionLimits::builder()
        .max_input_bytes(200_000)
        .max_box_count(32)
        .max_box_depth(3)
        .max_traf_count(1)
        .max_trun_count(4)
        .max_samples(512)
        .max_sample_table_bytes(100_000)
        .max_box_payload_bytes(200_000)
        .build()
        .expect("test limits are complete and non-zero")
}

/// Создаёт video expectation с authoritative manifest timing.
pub(super) fn video_expectation(
    base_decode_time: u64,
    defaults: FragmentSampleDefaults,
) -> FragmentTrackExpectation {
    expectation(
        base_decode_time,
        FragmentRapRequirement::RequireProvenVideoRandomAccess,
        defaults,
    )
}

/// Создаёт audio expectation без fake RAP requirement.
pub(super) fn audio_expectation(
    base_decode_time: u64,
    defaults: FragmentSampleDefaults,
) -> FragmentTrackExpectation {
    expectation(
        base_decode_time,
        FragmentRapRequirement::NotRequiredForAudio,
        defaults,
    )
}

/// Группирует typed expectation для captured track ID 1.
fn expectation(
    base_decode_time: u64,
    rap_requirement: FragmentRapRequirement,
    defaults: FragmentSampleDefaults,
) -> FragmentTrackExpectation {
    FragmentTrackExpectation::new(
        FragmentTrackId::new(NonZeroU32::new(1).expect("track id is non-zero")),
        FragmentBaseDecodeTime::new(base_decode_time),
        rap_requirement,
        defaults,
    )
}

/// Выполняет inspection с never-cancelled callback.
pub(super) fn inspect<'input>(
    input: &'input [u8],
    expectation: FragmentTrackExpectation,
) -> Result<NormalizedFragmentPlan<'input>, FragmentInspectionError> {
    inspect_with(input, expectation, &limits(), &|| false)
}

/// Выполняет inspection с injected limits/cancellation.
pub(super) fn inspect_with<'input>(
    input: &'input [u8],
    expectation: FragmentTrackExpectation,
    limits: &FragmentInspectionLimits,
    cancellation: &dyn Fn() -> bool,
) -> Result<NormalizedFragmentPlan<'input>, FragmentInspectionError> {
    inspect_with_semantics(
        input,
        FragmentCompositionOffsetSemantics::PiffSigned32Bit,
        expectation,
        limits,
        cancellation,
    )
}

/// Выполняет inspection с явно выбранной контейнерной семантикой.
pub(super) fn inspect_with_semantics<'input>(
    input: &'input [u8],
    composition_offset_semantics: FragmentCompositionOffsetSemantics,
    expectation: FragmentTrackExpectation,
    limits: &FragmentInspectionLimits,
    cancellation: &dyn Fn() -> bool,
) -> Result<NormalizedFragmentPlan<'input>, FragmentInspectionError> {
    let request = FragmentInspectionRequest::new(
        input,
        composition_offset_semantics,
        expectation,
        limits,
        cancellation,
    );
    inspect_media_fragment(&request)
}

/// Находит единственный box по fourcc в captured fixture.
pub(super) fn box_range(bytes: &[u8], fourcc: [u8; 4]) -> Range<usize> {
    let type_position = bytes
        .windows(4)
        .position(|window| window == fourcc)
        .expect("fixture contains requested box type");
    let box_start = type_position.checked_sub(4).expect("box has size field");
    let box_size = read_u32(bytes, box_start) as usize;
    box_start..box_start + box_size
}

/// Читает big-endian u32.
pub(super) fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("u32 field is in fixture"),
    )
}

/// Записывает big-endian u32.
pub(super) fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

/// Изменяет объявленный размер box-а на signed delta.
pub(super) fn adjust_box_size(bytes: &mut [u8], box_start: usize, delta: isize) {
    let old_size = read_u32(bytes, box_start) as isize;
    let new_size = old_size
        .checked_add(delta)
        .expect("test box size arithmetic");
    write_u32(
        bytes,
        box_start,
        u32::try_from(new_size).expect("test box size fits u32"),
    );
}

/// Вставляет child в конец `traf`, обновляя `traf` и `moof`.
pub(super) fn insert_traf_child(bytes: &mut Vec<u8>, child: &[u8]) {
    let moof = box_range(bytes, *b"moof");
    let traf = box_range(bytes, *b"traf");
    let insertion = traf.end;
    bytes.splice(insertion..insertion, child.iter().copied());
    adjust_box_size(bytes, traf.start, child.len() as isize);
    adjust_box_size(bytes, moof.start, child.len() as isize);
}

/// Возвращает minimal ISO box.
pub(super) fn atom(fourcc: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(payload.len() + 8).expect("test atom fits u32");
    let mut bytes = Vec::with_capacity(size as usize);
    bytes.extend_from_slice(&size.to_be_bytes());
    bytes.extend_from_slice(&fourcc);
    bytes.extend_from_slice(payload);
    bytes
}

/// Обновляет captured `trun.data_offset` до фактического начала `mdat` payload.
pub(super) fn repair_first_trun_data_offset(bytes: &mut [u8]) {
    let moof = box_range(bytes, *b"moof");
    let trun = box_range(bytes, *b"trun");
    let mdat_payload = moof.end + 8;
    write_u32(
        bytes,
        trun.start + 16,
        u32::try_from(mdat_payload - moof.start).expect("fixture offset fits u32"),
    );
}

/// Строит one-sample mutation exact high-video fixture с тем же первым payload.
pub(super) fn single_sample_video() -> Vec<u8> {
    let source = VIDEO_HIGH_FIRST;
    let moof = box_range(source, *b"moof");
    let traf = box_range(source, *b"traf");
    let trun = box_range(source, *b"trun");
    let first_sample_size = read_u32(source, trun.start + 28) as usize;
    let first_sample_row_end = trun.start + 36;
    let first_payload_start = moof.end + 8;
    let first_payload_end = first_payload_start + first_sample_size;

    let mut result = source[..first_sample_row_end].to_vec();
    write_u32(
        &mut result,
        trun.start,
        (first_sample_row_end - trun.start) as u32,
    );
    write_u32(&mut result, trun.start + 12, 1);
    let removed_trun_bytes = trun.end - first_sample_row_end;
    adjust_box_size(&mut result, traf.start, -(removed_trun_bytes as isize));
    adjust_box_size(&mut result, moof.start, -(removed_trun_bytes as isize));
    result.extend_from_slice(&atom(
        *b"mdat",
        &source[first_payload_start..first_payload_end],
    ));
    repair_first_trun_data_offset(&mut result);
    result
}

/// Удаляет per-sample поле из `trun`, сохраняя остальные строки без изменений.
pub(super) fn strip_trun_sample_field(bytes: &mut Vec<u8>, removed_flag: u32) {
    let moof = box_range(bytes, *b"moof");
    let traf = box_range(bytes, *b"traf");
    let trun = box_range(bytes, *b"trun");
    let flags = read_u32(bytes, trun.start + 8) & 0x00ff_ffff;
    assert_ne!(flags & removed_flag, 0, "field must be present");
    let sample_count = read_u32(bytes, trun.start + 12) as usize;
    let mut sample_fields_start = trun.start + 16;
    if flags & 0x000001 != 0 {
        sample_fields_start += 4;
    }
    if flags & 0x000004 != 0 {
        sample_fields_start += 4;
    }
    let field_flags = [0x000100, 0x000200, 0x000400, 0x000800];
    let active_fields: Vec<u32> = field_flags
        .into_iter()
        .filter(|field| flags & field != 0)
        .collect();
    let old_row_bytes = active_fields.len() * 4;
    let mut replacement = bytes[trun.start..sample_fields_start].to_vec();
    let new_flags = flags & !removed_flag;
    replacement[8..12].copy_from_slice(&new_flags.to_be_bytes());
    for sample_index in 0..sample_count {
        let row_start = sample_fields_start + sample_index * old_row_bytes;
        for (field_index, field_flag) in active_fields.iter().enumerate() {
            if *field_flag != removed_flag {
                let field_start = row_start + field_index * 4;
                replacement.extend_from_slice(&bytes[field_start..field_start + 4]);
            }
        }
    }
    let removed_bytes = trun.len() - replacement.len();
    let replacement_size = u32::try_from(replacement.len()).expect("test trun fits u32");
    replacement[0..4].copy_from_slice(&replacement_size.to_be_bytes());
    bytes.splice(trun.clone(), replacement);
    adjust_box_size(bytes, traf.start, -(removed_bytes as isize));
    adjust_box_size(bytes, moof.start, -(removed_bytes as isize));
    repair_first_trun_data_offset(bytes);
}

/// Удаляет `first_sample_flags` из captured `trun`.
pub(super) fn strip_first_sample_flags(bytes: &mut Vec<u8>) {
    let moof = box_range(bytes, *b"moof");
    let traf = box_range(bytes, *b"traf");
    let trun = box_range(bytes, *b"trun");
    let flags = read_u32(bytes, trun.start + 8) & 0x00ff_ffff;
    assert_ne!(flags & 0x000004, 0);
    let first_flags_offset = trun.start + 20;
    bytes.drain(first_flags_offset..first_flags_offset + 4);
    write_u32(bytes, trun.start, (trun.len() - 4) as u32);
    write_u32(bytes, trun.start + 8, flags & !0x000004);
    adjust_box_size(bytes, traf.start, -4);
    adjust_box_size(bytes, moof.start, -4);
    repair_first_trun_data_offset(bytes);
}

/// Удаляет `tfhd.default_sample_flags`.
pub(super) fn strip_tfhd_default_flags(bytes: &mut Vec<u8>) {
    let moof = box_range(bytes, *b"moof");
    let traf = box_range(bytes, *b"traf");
    let tfhd = box_range(bytes, *b"tfhd");
    let flags = read_u32(bytes, tfhd.start + 8) & 0x00ff_ffff;
    assert_ne!(flags & 0x000020, 0);
    let default_flags_offset = tfhd.end - 4;
    bytes.drain(default_flags_offset..tfhd.end);
    write_u32(bytes, tfhd.start, (tfhd.len() - 4) as u32);
    write_u32(bytes, tfhd.start + 8, flags & !0x000020);
    adjust_box_size(bytes, traf.start, -4);
    adjust_box_size(bytes, moof.start, -4);
    repair_first_trun_data_offset(bytes);
}

/// Добавляет `tfhd` duration/size defaults перед existing flags.
pub(super) fn add_tfhd_duration_and_size_defaults(bytes: &mut Vec<u8>, duration: u32, size: u32) {
    let moof = box_range(bytes, *b"moof");
    let traf = box_range(bytes, *b"traf");
    let tfhd = box_range(bytes, *b"tfhd");
    let flags = read_u32(bytes, tfhd.start + 8) & 0x00ff_ffff;
    let insertion = tfhd.end - 4;
    let mut fields = Vec::with_capacity(8);
    fields.extend_from_slice(&duration.to_be_bytes());
    fields.extend_from_slice(&size.to_be_bytes());
    bytes.splice(insertion..insertion, fields);
    adjust_box_size(bytes, tfhd.start, 8);
    write_u32(bytes, tfhd.start + 8, flags | 0x000018);
    adjust_box_size(bytes, traf.start, 8);
    adjust_box_size(bytes, moof.start, 8);
    repair_first_trun_data_offset(bytes);
}

/// Вставляет optional `tfdt` version 1.
pub(super) fn insert_tfdt(bytes: &mut Vec<u8>, base_decode_time: u64) {
    let mut payload = vec![1, 0, 0, 0];
    payload.extend_from_slice(&base_decode_time.to_be_bytes());
    insert_traf_child(bytes, &atom(*b"tfdt", &payload));
    repair_first_trun_data_offset(bytes);
}

/// Дублирует exact `traf` для multi-track/layout tests.
pub(super) fn duplicate_traf(bytes: &mut Vec<u8>) {
    let moof = box_range(bytes, *b"moof");
    let traf = box_range(bytes, *b"traf");
    let copy = bytes[traf].to_vec();
    bytes.splice(moof.end..moof.end, copy.iter().copied());
    adjust_box_size(bytes, moof.start, copy.len() as isize);
}

/// Дублирует exact `trun` внутри `traf`.
pub(super) fn duplicate_trun(bytes: &mut Vec<u8>) {
    let trun = box_range(bytes, *b"trun");
    let copy = bytes[trun].to_vec();
    insert_traf_child(bytes, &copy);
    repair_first_trun_data_offset(bytes);
}
