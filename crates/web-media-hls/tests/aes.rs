use aes::Aes128;
use cbc::{
    Encryptor,
    cipher::{BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7},
};
use hls_playlist_core::{
    HlsKeyDeclaration, HlsKeyMethod, HlsParseRequest, HlsParserLimits, HlsPlaylist,
    parse_hls_playlist,
};
use web_media_hls::{
    Aes128CbcDecryptError, Aes128InitializationVector, Aes128KeySource, ExtractorAesOverride,
    HlsKeyState, HlsKeyStateError, SecretAes128Key, decrypt_aes128_cbc_pkcs7,
};

fn key_declaration(method: HlsKeyMethod, iv: Option<[u8; 16]>) -> HlsKeyDeclaration {
    let playlist = format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:5\n\
         #EXT-X-KEY:METHOD=AES-128,URI=\"key\"{}\n\
         #EXTINF:5,\nsegment.ts\n#EXT-X-ENDLIST\n",
        iv.map(|bytes| format!(",IV=0x{}", hex(&bytes)))
            .unwrap_or_default()
    );
    let parsed = parse_hls_playlist(HlsParseRequest::new(
        playlist.as_bytes(),
        Some("https://example.invalid/media.m3u8"),
        HlsParserLimits::default(),
    ))
    .expect("valid playlist");
    let HlsPlaylist::Media(media) = parsed else {
        panic!("media");
    };
    let mut declaration = media.segments[0].key.clone().expect("key");
    declaration.method = method;
    declaration
}

#[test]
fn explicit_and_nonzero_derived_iv_follow_rfc() {
    let explicit = [7u8; 16];
    let mut state = HlsKeyState::default();
    state
        .apply(&key_declaration(HlsKeyMethod::Aes128, Some(explicit)), None)
        .expect("active");
    assert_eq!(
        state.active().expect("key").iv_for_media_segment(99),
        Aes128InitializationVector::explicit(explicit)
    );

    state
        .apply(&key_declaration(HlsKeyMethod::Aes128, None), None)
        .expect("active");
    assert_eq!(
        state.active().expect("key").iv_for_media_segment(42),
        Aes128InitializationVector::explicit([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 42])
    );
}

#[test]
fn rotation_override_precedence_and_none_reset_are_explicit() {
    let extractor = ExtractorAesOverride::new(
        Some("https://override.invalid/key?secret=yes"),
        Some("00112233445566778899aabbccddeeff"),
        Some("f"),
    )
    .expect("override");
    let mut state = HlsKeyState::default();
    state
        .apply(
            &key_declaration(HlsKeyMethod::Aes128, Some([9; 16])),
            Some(&extractor),
        )
        .expect("override wins");
    assert!(matches!(
        state.active().expect("active").source(),
        Aes128KeySource::Inline(_)
    ));
    assert_eq!(
        state.active().expect("active").iv_for_media_segment(1),
        Aes128InitializationVector::explicit([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 15])
    );

    let none = key_declaration(HlsKeyMethod::None, None);
    state.apply(&none, Some(&extractor)).expect("NONE resets");
    assert!(state.active().is_none());
}

#[test]
fn unsupported_crypto_and_key_length_are_typed_without_state_mutation() {
    assert_eq!(
        SecretAes128Key::from_key_file_bytes(&[0; 15]).unwrap_err(),
        HlsKeyStateError::InvalidKeyLength
    );
    let mut state = HlsKeyState::default();
    assert_eq!(
        state
            .apply(&key_declaration(HlsKeyMethod::SampleAes, None), None)
            .unwrap_err(),
        HlsKeyStateError::UnsupportedMethod
    );
    assert!(state.active().is_none());
}

#[test]
fn cbc_pkcs7_boundary_decrypts_and_rejects_length_and_padding() {
    let key_bytes = [0x11; 16];
    let iv_bytes = [0x22; 16];
    let plaintext = b"production-ready HLS bytes";
    let mut buffer = vec![0u8; plaintext.len() + 16];
    buffer[..plaintext.len()].copy_from_slice(plaintext);
    let ciphertext_length = Encryptor::<Aes128>::new((&key_bytes).into(), (&iv_bytes).into())
        .encrypt_padded::<Pkcs7>(&mut buffer, plaintext.len())
        .expect("encrypt fixture")
        .len();
    buffer.truncate(ciphertext_length);
    let key = SecretAes128Key::from_key_file_bytes(&key_bytes).expect("key");
    let decrypted = decrypt_aes128_cbc_pkcs7(
        &buffer,
        &key,
        Aes128InitializationVector::explicit(iv_bytes),
    )
    .expect("decrypt");
    assert_eq!(decrypted.expose_for_demux(), plaintext);

    assert_eq!(
        decrypt_aes128_cbc_pkcs7(
            &[0; 15],
            &key,
            Aes128InitializationVector::explicit(iv_bytes)
        )
        .unwrap_err(),
        Aes128CbcDecryptError::InvalidCiphertextLength
    );
    buffer[ciphertext_length - 1] ^= 0xff;
    assert_eq!(
        decrypt_aes128_cbc_pkcs7(
            &buffer,
            &key,
            Aes128InitializationVector::explicit(iv_bytes)
        )
        .unwrap_err(),
        Aes128CbcDecryptError::InvalidPkcs7Padding
    );
}

#[test]
fn encrypted_map_requires_explicit_iv() {
    let playlist = parse_hls_playlist(HlsParseRequest::new(
        b"#EXTM3U\n#EXT-X-TARGETDURATION:5\n\
          #EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n\
          #EXT-X-MAP:URI=\"init.mp4\"\n\
          #EXTINF:5,\nsegment.mp4\n#EXT-X-ENDLIST\n",
        Some("https://example.invalid/media.m3u8"),
        HlsParserLimits::default(),
    ))
    .expect("valid structure");
    let HlsPlaylist::Media(media) = playlist else {
        panic!("media");
    };
    assert_eq!(
        HlsKeyState::active_for_initialization_map(
            media.segments[0].initialization_map.as_ref().expect("map"),
            None,
        )
        .unwrap_err(),
        HlsKeyStateError::EncryptedMapRequiresExplicitIv
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
