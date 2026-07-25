//! Низкоуровневая passive data-socket policy без FTP command lifecycle.

use std::io;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use suppaftp::{FtpError, FtpResult};

/// Максимальная latency cooperative cancellation во время data read.
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Переключает готовый RETR socket на короткие bounded waits для cancellation polling.
pub(super) fn configure_read_poll(socket: &TcpStream, read_timeout: Duration) -> io::Result<()> {
    socket.set_read_timeout(Some(read_timeout.min(CANCELLATION_POLL_INTERVAL)))
}

/// Строит passive data socket с теми же connect/I/O bounds, что и control policy.
pub(super) fn stream_builder(
    connect_timeout: Duration,
    io_timeout: Duration,
) -> impl Fn(SocketAddr) -> FtpResult<TcpStream> + Send + Sync + 'static {
    move |address| {
        let stream = TcpStream::connect_timeout(&address, connect_timeout)
            .map_err(FtpError::ConnectionError)?;
        stream
            .set_read_timeout(Some(io_timeout))
            .map_err(FtpError::ConnectionError)?;
        stream
            .set_write_timeout(Some(io_timeout))
            .map_err(FtpError::ConnectionError)?;
        Ok(stream)
    }
}
