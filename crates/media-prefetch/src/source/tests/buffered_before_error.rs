//! Сессия 16: ошибка фоновой докачки не должна перебивать уже скачанные байты.

use super::*;

/// Worker успешно скачал 16 байт, а третий fetch упал. Читатель обязан получить
/// все 16 байт и только потом ошибку; следующий read после ошибки продолжает с
/// той же позиции (worker повторяет fetch с fetch-границы).
#[test]
fn buffered_bytes_are_delivered_before_worker_error() {
    let bytes = sample_bytes(64);
    let (inner, _handle) = FakeByteSource::seekable(bytes.clone());
    let inner = inner.with_fail_on_read_call(3);
    let mut source = start_test_source(Box::new(inner), test_config(8, 8, 32));
    let mut delivered = Vec::new();
    let mut output = [0; 4];

    let error = loop {
        match source.read(&mut output, &token()) {
            Ok(0) => panic!("EOF beforehand: {delivered:?}"),
            Ok(bytes_read) => delivered.extend_from_slice(&output[..bytes_read]),
            Err(error) => break error,
        }
    };

    assert_eq!(
        delivered,
        bytes[..16],
        "to error, exactly all downloaded bytes are given, without losses and holes"
    );
    assert!(matches!(
        error,
        SourceError::UnexpectedEof { offset: 16, .. }
    ));
    assert_eq!(source.position(), 16, "error does not shift position");

    let mut tail = Vec::new();
    loop {
        let bytes_read = source
            .read(&mut output, &token())
            .expect("After the error, the worker repeats the fetch from the same boundary.");
        if bytes_read == 0 {
            break;
        }
        tail.extend_from_slice(&output[..bytes_read]);
    }
    assert_eq!(tail, bytes[16..], "sequencing");
}
