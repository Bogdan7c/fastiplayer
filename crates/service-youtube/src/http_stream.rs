//! Потоковый HTTP transport для playback-only YouTube sources.

use std::io::Read;
use std::thread;

use anyhow::{Context, Result};
use bytes::Bytes;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use source_core::{HttpHeader, SourceRuntimeConfig};

use crate::YoutubeDirectStreamDescriptor;

/// Размер блока между HTTP reader-ом и streaming demux source.
const HTTP_READ_CHUNK_SIZE: usize = 64 * 1024;

/// Запускает transport worker для одного direct media URL.
pub(crate) fn spawn_http_fetcher(
    thread_name: &'static str,
    stream: YoutubeDirectStreamDescriptor,
    source_config: SourceRuntimeConfig,
    writer: symphonia_demux::StreamingByteWriter,
) -> Result<()> {
    thread::Builder::new()
        .name(thread_name.to_string())
        .spawn(move || {
            if let Err(fetch_error) = fetch_stream_to_writer(&stream, &source_config, &writer) {
                let fail_message = format!("{thread_name}: {fetch_error}");
                if let Err(writer_error) = writer.fail(fail_message) {
                    tracing::warn!(
                        error = %writer_error,
                        "Не удалось передать ошибку HTTP fetcher-а в streaming reader"
                    );
                }
            }
        })
        .with_context(|| format!("Не удалось запустить HTTP fetcher thread: {thread_name}"))?;

    Ok(())
}

/// Качает response body и сохраняет прежнюю EOF/error policy writer-а.
fn fetch_stream_to_writer(
    stream: &YoutubeDirectStreamDescriptor,
    source_config: &SourceRuntimeConfig,
    writer: &symphonia_demux::StreamingByteWriter,
) -> Result<()> {
    tracing::info!(description = %stream.description, "HTTP streaming fetch started");
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(source_config.connect_timeout())
        .timeout(source_config.read_timeout())
        .build()
        .context("Не удалось создать reqwest blocking client")?;
    let headers = build_header_map(&stream.headers)?;
    let mut response = client
        .get(&stream.url)
        .headers(headers)
        .send()
        .context("HTTP запрос direct media stream не удался")?
        .error_for_status()
        .context("YouTube direct media stream вернул HTTP ошибку")?;
    let mut read_buffer = vec![0u8; HTTP_READ_CHUNK_SIZE];

    loop {
        let bytes_read = response
            .read(&mut read_buffer)
            .context("Ошибка чтения HTTP stream")?;
        if bytes_read == 0 {
            writer.finish()?;
            tracing::info!(description = %stream.description, "HTTP streaming fetch finished");
            return Ok(());
        }
        writer.send_chunk(Bytes::copy_from_slice(&read_buffer[..bytes_read]))?;
    }
}

/// Конвертирует normalized service headers в transport-specific map.
fn build_header_map(headers: &[HttpHeader]) -> Result<HeaderMap> {
    let mut header_map = HeaderMap::new();
    for header in headers {
        let header_name = HeaderName::from_bytes(header.name.as_bytes())
            .with_context(|| format!("Некорректный HTTP header name от yt-dlp: {}", header.name))?;
        let header_value = HeaderValue::from_str(&header.value).with_context(|| {
            format!(
                "Некорректное значение HTTP header от yt-dlp: {}",
                header.name
            )
        })?;
        header_map.insert(header_name, header_value);
    }
    Ok(header_map)
}
