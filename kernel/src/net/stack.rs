/// TCP/IP network stack built on smoltcp.
///
/// Provides:
/// - DHCP for automatic IP configuration
/// - TCP socket creation and I/O
/// - UDP socket creation and I/O (for DNS)
use alloc::vec;
use core::sync::atomic::{AtomicU16, Ordering};

use smoltcp::iface::{Config, Interface, SocketSet, SocketHandle};
use smoltcp::socket::tcp::{self, Socket as TcpSocket};
use smoltcp::socket::udp::Socket as UdpSocket;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Address, Ipv4Cidr};

use super::device::SmoltcpDevice;

/// Monotonic ephemeral port counter (wraps within 49152..65535 range).
static EPHEMERAL_PORT: AtomicU16 = AtomicU16::new(49152);

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

    /// Get the current timestamp for smoltcp (calibrated TSC).
    fn now() -> Instant {
        let ms = crate::arch::x86_64::timer::monotonic_ms();
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

        // Pick an ephemeral local port from monotonic counter
        let port_offset = EPHEMERAL_PORT.fetch_add(1, Ordering::Relaxed);
        let local_port = 49152 + (port_offset % 16384);

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

    // ---- UDP support (for DNS) ----

    /// Add a UDP socket to the socket set.
    pub fn add_udp_socket(&mut self, socket: UdpSocket<'static>) -> SocketHandle {
        self.sockets.add(socket)
    }

    /// Bind a UDP socket to a local port.
    pub fn udp_bind(&mut self, handle: SocketHandle, port: u16) -> Result<(), ()> {
        let socket = self.sockets.get_mut::<UdpSocket>(handle);
        socket.bind(port).map_err(|_| ())
    }

    /// Send a UDP datagram.
    pub fn udp_send(
        &mut self,
        handle: SocketHandle,
        data: &[u8],
        endpoint: IpEndpoint,
    ) -> Result<(), ()> {
        let socket = self.sockets.get_mut::<UdpSocket>(handle);
        socket.send_slice(data, endpoint).map_err(|_| ())?;
        self.poll();
        Ok(())
    }

    /// Receive a UDP datagram. Returns the number of bytes received, or None.
    pub fn udp_recv(&mut self, handle: SocketHandle, buf: &mut [u8]) -> Option<usize> {
        let socket = self.sockets.get_mut::<UdpSocket>(handle);
        match socket.recv_slice(buf) {
            Ok((n, _endpoint)) => Some(n),
            Err(_) => None,
        }
    }

    /// Remove a socket from the socket set.
    pub fn remove_socket(&mut self, handle: SocketHandle) {
        self.sockets.remove(handle);
    }

    /// Get the next ephemeral port number.
    pub fn next_ephemeral_port(&self) -> u16 {
        let offset = EPHEMERAL_PORT.fetch_add(1, Ordering::Relaxed);
        49152 + (offset % 16384)
    }
}
