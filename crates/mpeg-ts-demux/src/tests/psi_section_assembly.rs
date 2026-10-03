//! Сборка PSI-секций (PAT/PMT) из TS-пакетов: секции, разрезанные между
//! пакетами, собираются ровно один раз, а битые формы дают typed ошибку.

use super::*;
use crate::psi::PsiSectionAssembler;

/// Валидная PAT-секция (без pointer_field), вырезанная из настоящего TS fixture-а.
fn valid_pat_section() -> Vec<u8> {
    let bytes = TsFixtureBuilder::new().pat(&[(1, PMT_PID)], 3).finish();
    let packet = &bytes[..188];
    let adaptation_length = usize::from(packet[4]);
    let payload = &packet[5 + adaptation_length..];
    let section = &payload[1..];
    let section_length = 3 + (((usize::from(section[1]) & 0x0f) << 8) | usize::from(section[2]));
    section[..section_length].to_vec()
}

/// Payload первого пакета секции: pointer_field = 0, затем начало секции.
fn unit_start_payload(section_part: &[u8]) -> Vec<u8> {
    let mut payload = vec![0];
    payload.extend_from_slice(section_part);
    payload
}

#[test]
fn section_split_between_packets_is_reassembled_exactly_once() {
    let section = valid_pat_section();
    // Разрез внутри 3-байтного заголовка: длина секции ещё неизвестна.
    for split in [1, 2, 5, section.len() - 1] {
        let mut assembler = PsiSectionAssembler::default();
        let first = assembler
            .push(true, &unit_start_payload(&section[..split]))
            .expect("first part is accepted");
        assert!(first.is_empty(), "split {split}: секция ещё не завершена");
        let completed = assembler
            .push(false, &section[split..])
            .expect("continuation completes section");
        assert_eq!(completed, vec![section.clone()], "split {split}");
        let (version, programs) = parse_pat(&completed[0]).expect("reassembled PAT parses");
        assert_eq!((version, programs[0].pmt_pid), (3, PMT_PID));
    }
}

#[test]
fn stuffing_after_section_is_ignored() {
    let section = valid_pat_section();
    let mut payload = unit_start_payload(&section);
    payload.extend_from_slice(&[0xff; 16]);
    let completed = PsiSectionAssembler::default()
        .push(true, &payload)
        .expect("stuffing bytes are not a section");
    assert_eq!(completed, vec![section]);
}

#[test]
fn malformed_pointer_and_section_shapes_are_typed_errors() {
    let section = valid_pat_section();
    let mut too_long = section.clone();
    // section_length = 0x0fff превышает MPEG-TS bound 1021.
    too_long[1] |= 0x0f;
    too_long[2] = 0xff;
    let mut corrupted_crc = section.clone();
    let last = corrupted_crc.len() - 1;
    corrupted_crc[last] ^= 1;
    let cases: [(&str, Vec<u8>, &str); 4] = [
        ("missing pointer", Vec::new(), "без pointer_field"),
        (
            "pointer beyond payload",
            vec![10, 0, 0],
            "pointer_field выходит за payload",
        ),
        (
            "section length",
            unit_start_payload(&too_long),
            "section_length вне",
        ),
        (
            "crc",
            unit_start_payload(&corrupted_crc),
            "CRC32 не совпадает",
        ),
    ];
    for (case, payload, expected) in cases {
        let error = PsiSectionAssembler::default()
            .push(true, &payload)
            .expect_err("malformed PSI must be rejected");
        assert!(error.to_string().contains(expected), "{case}: {error}");
    }
}

#[test]
fn new_section_before_previous_completes_is_rejected() {
    let section = valid_pat_section();
    let mut assembler = PsiSectionAssembler::default();
    assembler
        .push(true, &unit_start_payload(&section[..5]))
        .expect("first part is accepted");
    // pointer_field = 0: новая секция начинается, а предыдущая не дописана.
    let error = assembler
        .push(true, &unit_start_payload(&section))
        .expect_err("overlapping sections must be rejected");
    assert!(
        error.to_string().contains("до завершения предыдущей"),
        "{error}"
    );
}

#[test]
fn crc_failure_resets_assembler_for_the_next_valid_section() {
    let section = valid_pat_section();
    let mut corrupted = section.clone();
    let last = corrupted.len() - 1;
    corrupted[last] ^= 1;
    let mut assembler = PsiSectionAssembler::default();
    assembler
        .push(true, &unit_start_payload(&corrupted))
        .expect_err("corrupted CRC is rejected");
    let completed = assembler
        .push(true, &unit_start_payload(&section))
        .expect("assembler recovers on the next section");
    assert_eq!(completed, vec![section]);
}
