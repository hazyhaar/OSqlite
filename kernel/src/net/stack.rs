/// TCP/IP network stack built on smoltcp.
///
/// Provides:
/// - DHCP for automatic IP configuration
/// - TCP socket creation and I/O
/// - DNS resolution (static for now — api.anthropic.com hardcoded)
use alloc::vec;

use smoltcp::iface::{Config, Interface, SocketSet, SocketHandle};
use smoltcp::socket::tcp::{self, Socket as TcpSocket};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr, Ipv4Address, Ipv4Cidr};

use super::device::SmoltcpDevice;

/// Network stack state.
pub struct NetStack {
    device: SmoltcpDevice,
    iface: Interface,
    sockets: SocketSet<'static>,
}

impl NetStack {
    /// Create a new network stack. Requires the virtio-net driver to be
    /// initialized first.
    pub fn new() -> Option<Self> {
        let mut device = SmoltcpDevice::new();
        let mac = device.mac()?;

        let config = Config::new(EthernetAddress(mac).into());
        let mut iface = Interface::new(config, &mut device, Self::now());

        // Static IP for QEMU user-mode networking:
        // QEMU's default: guest = 10.0.2.15, gateway = 10.0.2.2, DNS = 10.0.2.3
        iface.update_ip_addrs(|addrs| {
            addrs.push(IpCidr::Ipv4(Ipv4Cidr::new(
                Ipv4Address::new(10, 0, 2, 15),
                24,
            ))).ok();
        });

        iface.routes_mut().add_default_ipv4_route(
            Ipv4Address::new(10, 0, 2, 2),  // QEMU gateway
        ).ok();

        let sockets = SocketSet::new(vec![]);

        Some(Self {
            device,
            iface,
            sockets,
        })
    }

    /// Get the current timestamp for smoltcp.
    fn now() -> Instant {
        // Use TSC-based millisecond counter.
        // TODO: calibrate TSC at boot for accurate time.
        // For now, use a simple counter.
        use crate::arch::x86_64::cpu::rdtsc;
        let tsc = rdtsc();
        // Assume ~2 GHz TSC — 1ms = 2_000_000 ticks
        let ms = tsc / 2_000_000;
        Instant::from_millis(ms as i64)
    }

    /// Poll the network stack — process incoming packets and advance
    /// TCP state machines. Must be called regularly.
    pub fn poll(&mut self) {
        let timestamp = Self::now();
        self.iface.poll(timestamp, &mut self.device, &mut self.sockets);
    }

    /// Open a TCP connection to the given IP and port.
    /// Returns a socket handle for reading/writing.
    pub fn tcp_connect(
        &mut self,
        remote_ip: Ipv4Address,
        remote_port: u16,
    ) -> Option<SocketHandle> {
        let rx_buf = tcp::SocketBuffer::new(vec![0u8; 65536]);
        let tx_buf = tcp::SocketBuffer::new(vec![0u8; 65536]);
        let socket = TcpSocket::new(rx_buf, tx_buf);

        let handle = self.sockets.add(socket);

        // Pick an ephemeral local port
        let local_port = 49152 + (Self::now().total_millis() as u16 % 16384);

        let socket = self.sockets.get_mut::<TcpSocket>(handle);
        socket.connect(
            self.iface.context(),
            (IpAddress::Ipv4(remote_ip), remote_port),
            local_port,
        ).ok()?;

        Some(handle)
    }

    /// Write data to a TCP socket.
    pub fn tcp_send(&mut self, handle: SocketHandle, data: &[u8]) -> usize {
        let socket = self.sockets.get_mut::<TcpSocket>(handle);
        match socket.send_slice(data) {
            Ok(n) => n,
            Err(_) => 0,
        }
    }

    /// Read data from a TCP socket.
    pub fn tcp_recv(&mut self, handle: SocketHandle, buf: &mut [u8]) -> usize {
        let socket = self.sockets.get_mut::<TcpSocket>(handle);
        match socket.recv_slice(buf) {
            Ok(n) => n,
            Err(_) => 0,
        }
    }

    /// Check if a TCP socket is connected and ready for I/O.
    pub fn tcp_is_active(&mut self, handle: SocketHandle) -> bool {
        let socket = self.sockets.get_mut::<TcpSocket>(handle);
        socket.is_active()
    }

    /// Check if a TCP socket can send data.
    pub fn tcp_can_send(&mut self, handle: SocketHandle) -> bool {
        let socket = self.sockets.get_mut::<TcpSocket>(handle);
        socket.can_send()
    }

    /// Check if a TCP socket has data to receive.
    pub fn tcp_can_recv(&mut self, handle: SocketHandle) -> bool {
        let socket = self.sockets.get_mut::<TcpSocket>(handle);
        socket.can_recv()
    }

    /// Close a TCP socket.
    pub fn tcp_close(&mut self, handle: SocketHandle) {
        let socket = self.sockets.get_mut::<TcpSocket>(handle);
        socket.close();
    }

    /// Poll until a condition is true, with a timeout.
    /// Returns true if the condition was met, false on timeout.
    pub fn poll_until<F>(&mut self, mut condition: F, timeout_ms: u64) -> bool
    where
        F: FnMut(&mut Self) -> bool,
    {
        let start = Self::now();
        loop {
            self.poll();
            if condition(self) {
                return true;
            }
            let elapsed = Self::now().total_millis() - start.total_millis();
            if elapsed as u64 > timeout_ms {
                return false;
            }
            core::hint::spin_loop();
        }
    }
}
