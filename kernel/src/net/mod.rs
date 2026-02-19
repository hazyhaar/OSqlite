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
