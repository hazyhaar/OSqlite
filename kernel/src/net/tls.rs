/// Blocking embedded-io adapter for smoltcp TCP sockets + TLS wrapper.
///
/// Bridges smoltcp's poll-based TCP API to the `embedded_io::Read` / `Write`
/// traits required by `embedded-tls`.
use smoltcp::iface::SocketHandle;

use super::stack::NetStack;

/// Error type for TCP stream operations.
#[derive(Debug)]
pub enum TcpError {
    Closed,
    Timeout,
}

impl core::fmt::Display for TcpError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TcpError::Closed => write!(f, "connection closed"),
            TcpError::Timeout => write!(f, "connection timeout"),
        }
    }
}

impl core::error::Error for TcpError {}

impl embedded_io::Error for TcpError {
    fn kind(&self) -> embedded_io::ErrorKind {
        match self {
            TcpError::Closed => embedded_io::ErrorKind::ConnectionReset,
            TcpError::Timeout => embedded_io::ErrorKind::TimedOut,
        }
    }
}

/// Blocking TCP stream over smoltcp.
///
/// Wraps a smoltcp SocketHandle + NetStack reference, implementing
/// `embedded_io::Read` and `embedded_io::Write` with spin-loop polling.
pub struct TcpStream<'a> {
    pub(crate) net: &'a mut NetStack,
    pub(crate) handle: SocketHandle,
}

impl<'a> TcpStream<'a> {
    pub fn new(net: &'a mut NetStack, handle: SocketHandle) -> Self {
        Self { net, handle }
    }
}

impl embedded_io::ErrorType for TcpStream<'_> {
    type Error = TcpError;
}

impl embedded_io::Read for TcpStream<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let start = crate::arch::x86_64::timer::monotonic_ms();
        loop {
            self.net.poll();
            if self.net.tcp_can_recv(self.handle) {
                let n = self.net.tcp_recv(self.handle, buf);
                if n > 0 {
                    return Ok(n);
                }
            }
            if !self.net.tcp_is_active(self.handle) {
                return Ok(0); // EOF
            }
            // 30 second timeout
            let elapsed = crate::arch::x86_64::timer::monotonic_ms() - start;
            if elapsed > 30_000 {
                return Err(TcpError::Timeout);
            }
            core::hint::spin_loop();
        }
    }
}

impl embedded_io::Write for TcpStream<'_> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let start = crate::arch::x86_64::timer::monotonic_ms();
        loop {
            self.net.poll();
            if self.net.tcp_can_send(self.handle) {
                let n = self.net.tcp_send(self.handle, buf);
                if n > 0 {
                    return Ok(n);
                }
            }
            if !self.net.tcp_is_active(self.handle) {
                return Err(TcpError::Closed);
            }
            let elapsed = crate::arch::x86_64::timer::monotonic_ms() - start;
            if elapsed > 30_000 {
                return Err(TcpError::Timeout);
            }
            core::hint::spin_loop();
        }
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.net.poll();
        Ok(())
    }
}
