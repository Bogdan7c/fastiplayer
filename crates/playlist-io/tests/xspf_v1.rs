use std::path::PathBuf;

use bounded_xml_reader::{XmlBudgetsBuilder, XmlReadError};
use playlist_core::{
    DurableReopenLocator, ForeignPathEncoding, ForeignPathPlatform, ForeignPlatformPath,
    LocalLocator,
};
use playlist_io::{
    FASTIPLAYER_XSPF_EXTENSION_NAMESPACE, XspfDocumentSource, XspfExportIneligible,
    XspfExportLocation, XspfParseError, XspfParseErrorKind, XspfParseRequest, XspfParserLimits,
    XspfPlaylist, parse_xspf_document,
};

/// Разбирает network XSPF с default bounded profile.
fn parse_network(document: &str) -> Result<XspfPlaylist, XspfParseError> {
    parse_xspf_document(XspfParseRequest::new(
        document.as_bytes(),
        XspfDocumentSource::network("https://example.invalid/lists/main.xspf")
            .expect("valid network document source"),
        XspfParserLimits::default(),
    ))
}

/// Возвращает URI candidate по track/location indexes.
fn location_uri(playlist: &XspfPlaylist, track_index: usize, location_index: usize) -> &str {
    playlist.tracks()[track_index].location_candidates()[location_index].expose_uri_for_admission()
}

/// Строит маленький complete XML profile для focused bomb tests.
fn xml_limits(
    maximum_depth: usize,
    maximum_attributes_per_element: usize,
    maximum_namespace_declarations: usize,
) -> XspfParserLimits {
    let xml_budgets = XmlBudgetsBuilder::new()
        .maximum_document_bytes(32 * 1024)
        .maximum_depth(maximum_depth)
        .maximum_tokens(1_024)
        .maximum_attributes_per_element(maximum_attributes_per_element)
        .maximum_attribute_count(1_024)
        .maximum_attribute_bytes(16 * 1024)
        .maximum_namespace_declarations_per_element(maximum_namespace_declarations)
        .maximum_namespace_declaration_count(maximum_namespace_declarations)
        .maximum_namespace_bytes(4 * 1024)
        .maximum_text_bytes(16 * 1024)
        .build()
        .expect("every XML budget is named");
    XspfParserLimits::new(xml_budgets)
}

#[test]
fn official_file_and_network_examples_preserve_track_order() {
    let file_example = r#"<?xml version="1.0" encoding="UTF-8"?>
<playlist version="1" xmlns="http://xspf.org/ns/0/">
  <trackList>
    <track><location>file:///music/song_1.ogg</location></track>
    <track><location>file:///music/song_2.flac</location></track>
    <track><location>file:///music/song_3.mp3</location></track>
  </trackList>
</playlist>"#;
    let network_example = r#"<?xml version="1.0" encoding="UTF-8"?>
<playlist version="1" xmlns="http://xspf.org/ns/0/">
  <trackList>
    <track><location>http://example.net/song_1.ogg</location></track>
    <track><location>http://example.net/song_2.flac</location></track>
    <track><location>http://example.com/song_3.mp3</location></track>
  </trackList>
</playlist>"#;

    let file_playlist = parse_network(file_example).expect("official file example");
    let network_playlist = parse_network(network_example).expect("official network example");

    assert_eq!(file_playlist.tracks().len(), 3);
    assert_eq!(
        location_uri(&file_playlist, 1, 0),
        "file:///music/song_2.flac"
    );
    assert_eq!(network_playlist.tracks().len(), 3);
    assert_eq!(
        location_uri(&network_playlist, 2, 0),
        "http://example.com/song_3.mp3"
    );
}

#[test]
fn nested_xml_base_is_inherited_and_percent_encodes_unicode() {
    let document = r#"<x:playlist xmlns:x="http://xspf.org/ns/0/" version="1"
        xml:base="https://media.example/root/">
      <x:trackList xml:base="albums/">
        <x:track xml:base="first/">
          <x:location>My%20Song.flac</x:location>
          <x:location xml:base="../second/">rosé.flac</x:location>
        </x:track>
      </x:trackList>
    </x:playlist>"#;

    let playlist = parse_network(document).expect("nested XML Base");

    assert_eq!(
        location_uri(&playlist, 0, 0),
        "https://media.example/root/albums/first/My%20Song.flac"
    );
    assert_eq!(
        location_uri(&playlist, 0, 1),
        "https://media.example/root/albums/second/ros%C3%A9.flac"
    );
}

#[test]
fn local_document_uri_is_the_initial_base() {
    let document = br#"<playlist xmlns="http://xspf.org/ns/0/" version="1">
      <trackList><track><location>../audio/song.flac</location></track></trackList>
    </playlist>"#;
    let playlist = parse_xspf_document(XspfParseRequest::new(
        document,
        XspfDocumentSource::local("/home/listener/playlists/main.xspf"),
        XspfParserLimits::default(),
    ))
    .expect("absolute local document base");

    assert_eq!(
        location_uri(&playlist, 0, 0),
        "file:///home/listener/audio/song.flac"
    );
}

#[test]
fn multiple_and_missing_locations_do_not_trigger_parser_side_choice() {
    let document = r#"<playlist xmlns="http://xspf.org/ns/0/" version="1">
      <trackList>
        <track>
          <location>custom:first</location>
          <location>https://example.invalid/second.mp3</location>
        </track>
        <track><title>Metadata only</title></track>
      </trackList>
    </playlist>"#;

    let playlist = parse_network(document).expect("ordered candidates");

    assert_eq!(playlist.tracks()[0].location_candidates().len(), 2);
    assert_eq!(location_uri(&playlist, 0, 0), "custom:first");
    assert_eq!(
        location_uri(&playlist, 0, 1),
        "https://example.invalid/second.mp3"
    );
    assert!(playlist.tracks()[1].location_candidates().is_empty());
}

#[test]
fn metadata_and_predefined_entities_are_decoded_without_playback_span() {
    let document = r#"<playlist xmlns="http://xspf.org/ns/0/" version="1">
      <trackList>
        <track>
          <location>song.flac?artist=A&amp;album=B</location>
          <title>A &amp; B &lt;demo&gt; &quot;mix&quot; &apos;cut&apos;</title>
          <creator>Artist</creator>
          <album>Album</album>
          <trackNum>7</trackNum>
          <duration>271066</duration>
        </track>
      </trackList>
    </playlist>"#;

    let playlist = parse_network(document).expect("legal predefined entities");
    let track = &playlist.tracks()[0];

    assert_eq!(
        location_uri(&playlist, 0, 0),
        "https://example.invalid/lists/song.flac?artist=A&album=B"
    );
    assert_eq!(track.title(), Some("A & B <demo> \"mix\" 'cut'"));
    assert_eq!(track.creator(), Some("Artist"));
    assert_eq!(track.album(), Some("Album"));
    assert_eq!(track.track_number().expect("track number").value(), 7);
    assert_eq!(
        track
            .duration_hint()
            .expect("duration hint")
            .as_duration()
            .as_millis(),
        271_066
    );
}

#[test]
fn exact_namespace_version_track_list_order_and_cardinality_are_enforced() {
    let wrong_namespace = r#"<playlist xmlns="urn:not-xspf" version="1"><trackList/></playlist>"#;
    let wrong_version =
        r#"<playlist xmlns="http://xspf.org/ns/0/" version="0"><trackList/></playlist>"#;
    let duplicate_track_list = r#"<playlist xmlns="http://xspf.org/ns/0/" version="1">
      <trackList/><trackList/>
    </playlist>"#;
    let wrong_track_order = r#"<playlist xmlns="http://xspf.org/ns/0/" version="1">
      <trackList><track><title>T</title><location>a.mp3</location></track></trackList>
    </playlist>"#;

    assert_eq!(
        parse_network(wrong_namespace)
            .expect_err("namespace")
            .kind(),
        &XspfParseErrorKind::UnexpectedNamespace
    );
    assert_eq!(
        parse_network(wrong_version).expect_err("version").kind(),
        &XspfParseErrorKind::UnsupportedVersion
    );
    assert_eq!(
        parse_network(duplicate_track_list)
            .expect_err("duplicate trackList")
            .kind(),
        &XspfParseErrorKind::DuplicateChild
    );
    assert_eq!(
        parse_network(wrong_track_order)
            .expect_err("track child order")
            .kind(),
        &XspfParseErrorKind::ChildOrderViolation
    );
}

#[test]
fn doctype_external_and_custom_entities_are_rejected_by_xml_boundary() {
    let doctype = r#"<!DOCTYPE playlist SYSTEM "https://attacker.invalid/xspf.dtd">
      <playlist xmlns="http://xspf.org/ns/0/" version="1"><trackList/></playlist>"#;
    let custom_reference = r#"<playlist xmlns="http://xspf.org/ns/0/" version="1">
      <trackList><track><title>&custom;</title></track></trackList>
    </playlist>"#;

    assert!(matches!(
        parse_network(doctype).expect_err("DOCTYPE").kind(),
        XspfParseErrorKind::Xml(XmlReadError::DocTypeForbidden)
    ));
    assert!(matches!(
        parse_network(custom_reference)
            .expect_err("custom entity")
            .kind(),
        XspfParseErrorKind::Xml(XmlReadError::CustomEntityForbidden)
    ));
}

#[test]
fn namespace_duplicate_attribute_and_depth_bombs_fail_typed() {
    let namespace_flood = r#"<playlist xmlns="http://xspf.org/ns/0/" version="1"
      xmlns:a="urn:a" xmlns:b="urn:b"><trackList/></playlist>"#;
    let duplicate_attribute = r#"<playlist xmlns="http://xspf.org/ns/0/"
      version="1" version="1"><trackList/></playlist>"#;
    let deep_document = r#"<playlist xmlns="http://xspf.org/ns/0/" version="1">
      <extension application="urn:unknown"><a xmlns="urn:a"><b><c/></b></a></extension>
      <trackList/>
    </playlist>"#;

    let namespace_error = parse_xspf_document(XspfParseRequest::new(
        namespace_flood.as_bytes(),
        XspfDocumentSource::local("/tmp/list.xspf"),
        xml_limits(16, 8, 1),
    ))
    .expect_err("namespace flood");
    assert!(matches!(
        namespace_error.kind(),
        XspfParseErrorKind::Xml(XmlReadError::NamespaceDeclarationsPerElementExceeded { .. })
    ));

    assert!(matches!(
        parse_network(duplicate_attribute)
            .expect_err("duplicate attr")
            .kind(),
        XspfParseErrorKind::Xml(XmlReadError::MalformedAttribute)
    ));

    let depth_error = parse_xspf_document(XspfParseRequest::new(
        deep_document.as_bytes(),
        XspfDocumentSource::local("/tmp/list.xspf"),
        xml_limits(4, 8, 8),
    ))
    .expect_err("depth bomb");
    assert!(matches!(
        depth_error.kind(),
        XspfParseErrorKind::Xml(XmlReadError::DepthExceeded { .. })
    ));
}

#[test]
fn raw_spaces_and_malformed_percent_encoding_are_rejected() {
    let raw_space = r#"<playlist xmlns="http://xspf.org/ns/0/" version="1">
      <trackList><track><location>My Song.flac</location></track></trackList>
    </playlist>"#;
    let bad_percent = r#"<playlist xmlns="http://xspf.org/ns/0/" version="1">
      <trackList><track><location>bad%2G.flac</location></track></trackList>
    </playlist>"#;

    assert_eq!(
        parse_network(raw_space).expect_err("raw space").kind(),
        &XspfParseErrorKind::InvalidUri
    );
    assert_eq!(
        parse_network(bad_percent)
            .expect_err("malformed percent")
            .kind(),
        &XspfParseErrorKind::InvalidUri
    );
}

#[test]
fn model_limits_cannot_claim_more_than_domain_capacity() {
    let document =
        br#"<playlist xmlns="http://xspf.org/ns/0/" version="1"><trackList/></playlist>"#;
    let over_capacity = playlist_core::MAX_PLAYLIST_ITEMS + 1;
    let track_error = parse_xspf_document(XspfParseRequest::new(
        document,
        XspfDocumentSource::local("/tmp/list.xspf"),
        XspfParserLimits::default().with_maximum_tracks(over_capacity),
    ))
    .expect_err("track cap above domain capacity");
    assert_eq!(
        track_error.kind(),
        &XspfParseErrorKind::TrackLimitExceedsDomainCapacity
    );

    let group_error = parse_xspf_document(XspfParseRequest::new(
        document,
        XspfDocumentSource::local("/tmp/list.xspf"),
        XspfParserLimits::default().with_maximum_groups(over_capacity),
    ))
    .expect_err("group cap above domain capacity");
    assert_eq!(
        group_error.kind(),
        &XspfParseErrorKind::GroupLimitExceedsDomainCapacity
    );
}

#[test]
fn fastiplayer_extension_uses_one_playlist_level_group_record() {
    let document = format!(
        r#"<playlist xmlns="http://xspf.org/ns/0/" xmlns:rp="{extension}" version="1">
          <extension application="{extension}" xml:base="https://service.example/">
            <rp:group firstTrack="1" trackCount="2">
              <rp:location>course/42</rp:location>
            </rp:group>
          </extension>
          <trackList>
            <track><location>part-1.mp4</location></track>
            <track><location>part-2.mp4</location></track>
            <track><location>single.mp4</location></track>
          </trackList>
        </playlist>"#,
        extension = FASTIPLAYER_XSPF_EXTENSION_NAMESPACE,
    );

    let playlist = parse_network(&document).expect("known group extension");

    assert_eq!(playlist.groups().len(), 1);
    let group = &playlist.groups()[0];
    assert_eq!(group.first_track().get(), 1);
    assert_eq!(group.track_count().get(), 2);
    assert_eq!(
        group.root_location().expose_uri_for_admission(),
        "https://service.example/course/42"
    );
}

#[test]
fn invalid_or_overlapping_group_ranges_are_rejected_after_track_list() {
    let document = format!(
        r#"<playlist xmlns="http://xspf.org/ns/0/" xmlns:rp="{extension}" version="1">
          <extension application="{extension}">
            <rp:group firstTrack="1" trackCount="2"><rp:location>one</rp:location></rp:group>
            <rp:group firstTrack="2" trackCount="1"><rp:location>two</rp:location></rp:group>
          </extension>
          <trackList><track/><track/></trackList>
        </playlist>"#,
        extension = FASTIPLAYER_XSPF_EXTENSION_NAMESPACE,
    );

    assert_eq!(
        parse_network(&document)
            .expect_err("overlapping groups")
            .kind(),
        &XspfParseErrorKind::InvalidGroupRange
    );
}

#[test]
fn unknown_application_extension_is_bounded_but_never_executed() {
    let document = r#"<playlist xmlns="http://xspf.org/ns/0/" version="1">
      <extension application="urn:vendor:extension">
        <vendor:command xmlns:vendor="urn:vendor">do-not-run</vendor:command>
      </extension>
      <trackList><track/></trackList>
    </playlist>"#;

    let playlist = parse_network(document).expect("unknown extension is skipped");

    assert_eq!(playlist.tracks().len(), 1);
    assert!(playlist.groups().is_empty());
}

#[test]
fn export_location_percent_encodes_native_path_and_rejects_foreign_identity() {
    let native_locator = DurableReopenLocator::local(LocalLocator::Native(PathBuf::from(
        "/music/My Song №1.flac",
    )));
    let native_export =
        XspfExportLocation::from_durable_locator(&native_locator).expect("representable path");

    assert_eq!(
        native_export.as_uri(),
        "file:///music/My%20Song%20%E2%84%961.flac"
    );

    let foreign_locator =
        DurableReopenLocator::local(LocalLocator::Foreign(ForeignPlatformPath::new(
            ForeignPathPlatform::Linux,
            ForeignPathEncoding::Bytes(vec![b'/', 0xff]),
        )));
    assert_eq!(
        XspfExportLocation::from_durable_locator(&foreign_locator),
        Err(XspfExportIneligible::ForeignPlatformPath)
    );
}

#[test]
fn foreign_product_extension_keeps_standard_tracks_without_importing_groups() {
    let document = r#"<playlist xmlns="http://xspf.org/ns/0/" version="1">
      <extension application="urn:formerplayer:xspf:playlist-extension:1">
        <group xmlns="urn:formerplayer:xspf:playlist-extension:1" firstTrack="1" trackCount="2"/>
      </extension>
      <trackList><track><location>movie.mp4</location></track></trackList>
    </playlist>"#;
    let playlist = parse_network(document).expect("foreign extension remains unknown");
    assert!(playlist.groups().is_empty());
    assert_eq!(playlist.tracks().len(), 1);
    assert_eq!(
        location_uri(&playlist, 0, 0),
        "https://example.invalid/lists/movie.mp4"
    );
}
