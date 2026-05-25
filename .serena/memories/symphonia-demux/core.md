# Symphonia Demux Core

- `symphonia-demux` owns the concrete Symphonia `FormatReader` state and maps container read results into neutral `media_core::DemuxReadEvent` / typed `DemuxError`.
- `FormatReader::next_packet()` taxonomy follows Symphonia 0.6 docs: `Ok(Some(packet))` returns a packet, `Ok(None)` is EOF, `Err(ResetRequired)` is handled as `DemuxReadEvent::TracksChanged`, and all other errors are structural/fatal demux read errors.
- Defensive compatibility is preserved for `SymphoniaError::IoError(UnexpectedEof)`: it maps to `EndOfStream` for legacy/current tests.
- Do not treat `SymphoniaError::DecodeError` from `FormatReader::next_packet()` as recoverable corrupted packet skip. The original Symphonia reason should be preserved in `DemuxError::Parse`; e.g. `isomp4: no atom pending read` must fail immediately, not loop until `max_consecutive_corrupted_packets`.
- Recoverable bad packet skip belongs to decoder paths such as `audio` decoder `decode(packet)`, where Symphonia `DecodeError`/`IoError` can represent an invalid encoded packet.
- Focused coverage lives in `crates/symphonia-demux/src/symphonia_demuxer.rs` tests, including `decode_error_from_format_reader_is_parse_error_without_retry`, `unexpected_eof_error_is_kept_as_defensive_eof_fallback`, and `reset_required_refreshes_track_list_as_lifecycle_event`.