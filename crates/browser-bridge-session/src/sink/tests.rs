use super::*;
use browser_bridge_protocol::SessionKey;

fn key(text: &str) -> SessionRef {
    SessionRef {
        key: SessionKey::new(text).unwrap(),
        instance: crate::registry::SessionInstance(1),
    }
}

fn buffer_chunk<'a>(
    session: &'a SessionRef,
    generation: u32,
    source_buffer: u16,
    sequence: u64,
    payload: &'a [u8],
) -> SessionChunk<'a> {
    SessionChunk {
        session,
        source: ChunkSource::SourceBuffer(SourceBufferChunk {
            generation: StreamGeneration::new(generation).unwrap(),
            source_buffer: SourceBufferIndex::new(source_buffer).unwrap(),
            sequence: AppendSequence(sequence),
            payload,
        }),
    }
}

#[test]
fn counts_frames_and_bytes_separately_per_generation_and_source_buffer() {
    let sink = CountingByteSink::new();
    let session = key("tab-1");
    sink.accept(buffer_chunk(&session, 1, 0, 0, &[0; 10]));
    sink.accept(buffer_chunk(&session, 1, 0, 1, &[0; 5]));
    sink.accept(buffer_chunk(&session, 1, 1, 0, &[0; 7]));
    sink.accept(buffer_chunk(&session, 2, 0, 0, &[0; 3]));

    let snapshot = sink.snapshot();
    let counters = |generation: u32, source_buffer: u16| {
        snapshot.source_buffers[&(
            StreamGeneration::new(generation).unwrap(),
            SourceBufferIndex::new(source_buffer).unwrap(),
        )]
    };
    assert_eq!(
        counters(1, 0),
        StreamCounters {
            frames: 2,
            bytes: 15
        }
    );
    assert_eq!(
        counters(1, 1),
        StreamCounters {
            frames: 1,
            bytes: 7
        }
    );
    assert_eq!(
        counters(2, 0),
        StreamCounters {
            frames: 1,
            bytes: 3
        }
    );
    assert_eq!(snapshot.source_buffers.len(), 3);
}

#[test]
fn counts_range_chunks_per_request_and_records_ended_sessions() {
    let sink = CountingByteSink::new();
    let session = key("tab-2");
    for request in [4, 4, 9] {
        sink.accept(SessionChunk {
            session: &session,
            source: ChunkSource::Range(RangeChunk {
                request_id: RangeRequestId(request),
                offset: 0,
                payload: &[1, 2, 3, 4],
            }),
        });
    }
    sink.session_ended(&session);

    let snapshot = sink.snapshot();
    assert_eq!(
        snapshot.ranges[&RangeRequestId(4)],
        StreamCounters {
            frames: 2,
            bytes: 8
        }
    );
    assert_eq!(
        snapshot.ranges[&RangeRequestId(9)],
        StreamCounters {
            frames: 1,
            bytes: 4
        }
    );
    assert!(snapshot.source_buffers.is_empty());
    assert_eq!(snapshot.ended_sessions, vec![session]);
}
