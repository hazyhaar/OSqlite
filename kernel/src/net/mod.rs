/// Network stack — bridges the NIC driver to TCP/IP via smoltcp.
///
/// Architecture:
///   virtio-net driver (raw Ethernet frames)
///       ↓ ↑
///   SmoltcpDevice (implements smoltcp::phy::Device)
///       ↓ ↑
///   smoltcp Interface (ARP, IP, TCP)
///       ↓ ↑
///   TCP sockets (used by HTTP client, TLS, etc.)
mod device;
pub mod stack;

pub use stack::NetStack;

/// Global network stack instance (initialized during boot if virtio-net is present).
pub static NET_STACK: spin::Mutex<Option<NetStack>> = spin::Mutex::new(None);
