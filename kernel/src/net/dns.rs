/// Minimal DNS resolver over UDP.
///
/// Sends A-record queries to the QEMU default DNS forwarder (10.0.2.3)
/// and parses the response. Uses smoltcp's UDP socket support.
///
/// The DNS packet format follows RFC 1035:
/// - Header: 12 bytes (ID, flags, counts)
/// - Question: encoded hostname + type (A) + class (IN)
/// - Answer: name + type + class + TTL + rdlength + rdata (4 bytes for A)

use alloc::string::String;
use alloc::vec;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::udp::{self, Socket as UdpSocket};
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address};

use super::stack::NetStack;

/// DNS query port.
const DNS_PORT: u16 = 53;

/// QEMU's default DNS forwarder IP.
const QEMU_DNS: Ipv4Address = Ipv4Address::new(10, 0, 2, 3);

/// Maximum time to wait for a DNS response (ms).
const DNS_TIMEOUT_MS: u64 = 5_000;

/// DNS error types.
#[derive(Debug)]
pub enum DnsError {
    /// Hostname is too long or malformed.
    InvalidHostname,
    /// No UDP socket available.
    SocketError,
    /// DNS query timed out.
    Timeout,
    /// Server returned an error or no answer.
    NoAnswer,
    /// Response packet is malformed.
    MalformedResponse,
}

impl core::fmt::Display for DnsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DnsError::InvalidHostname => write!(f, "invalid hostname"),
            DnsError::SocketError => write!(f, "UDP socket error"),
            DnsError::Timeout => write!(f, "DNS query timeout"),
            DnsError::NoAnswer => write!(f, "no DNS answer"),
            DnsError::MalformedResponse => write!(f, "malformed DNS response"),
        }
    }
}

/// Simple DNS A-record cache entry.
struct CacheEntry {
    hostname: String,
    ip: Ipv4Address,
    /// Absolute time (monotonic ms) when this entry expires.
    expires_ms: u64,
}

/// DNS cache — small fixed-size LRU-ish cache.
const CACHE_SIZE: usize = 8;
static DNS_CACHE: spin::Mutex<[Option<CacheEntry>; CACHE_SIZE]> =
    spin::Mutex::new([const { None }; CACHE_SIZE]);

/// Resolve a hostname to an IPv4 address using DNS over UDP.
///
/// Checks the cache first, then sends a UDP query to QEMU's DNS forwarder.
pub fn resolve_a(net: &mut NetStack, hostname: &str) -> Result<Ipv4Address, DnsError> {
    // Check cache first
    let now_ms = crate::arch::x86_64::timer::monotonic_ms();
    {
        let cache = DNS_CACHE.lock();
        for entry in cache.iter().flatten() {
            if entry.hostname == hostname && entry.expires_ms > now_ms {
                return Ok(entry.ip);
            }
        }
    }

    // Build DNS query packet
    let query = build_query(hostname)?;

    // Create UDP socket
    let rx_buf = udp::PacketBuffer::new(
        vec![udp::PacketMetadata::EMPTY; 4],
        vec![0u8; 1024],
    );
    let tx_buf = udp::PacketBuffer::new(
        vec![udp::PacketMetadata::EMPTY; 4],
        vec![0u8; 1024],
    );
    let socket = UdpSocket::new(rx_buf, tx_buf);
    let handle = net.add_udp_socket(socket);

    // Bind to an ephemeral port
    let local_port = net.next_ephemeral_port();
    if net.udp_bind(handle, local_port).is_err() {
        net.remove_socket(handle);
        return Err(DnsError::SocketError);
    }

    // Send query
    let endpoint = IpEndpoint::new(IpAddress::Ipv4(QEMU_DNS), DNS_PORT);
    net.poll();
    if net.udp_send(handle, &query, endpoint).is_err() {
        net.remove_socket(handle);
        return Err(DnsError::SocketError);
    }

    // Wait for response
    let start = crate::arch::x86_64::timer::monotonic_ms();
    let mut resp_buf = [0u8; 512];
    let result = loop {
        net.poll();

        if let Some(n) = net.udp_recv(handle, &mut resp_buf) {
            break parse_response(&resp_buf[..n], hostname);
        }

        let elapsed = crate::arch::x86_64::timer::monotonic_ms() - start;
        if elapsed > DNS_TIMEOUT_MS {
            break Err(DnsError::Timeout);
        }
        core::hint::spin_loop();
    };

    net.remove_socket(handle);

    // Cache the result
    if let Ok((ip, ttl)) = result {
        let expires = now_ms + (ttl as u64 * 1000).min(300_000); // cap at 5 min
        let mut cache = DNS_CACHE.lock();
        // Find empty slot or oldest entry
        let mut slot = 0;
        for (i, entry) in cache.iter().enumerate() {
            match entry {
                None => { slot = i; break; }
                Some(e) if e.hostname == hostname => { slot = i; break; }
                Some(e) if e.expires_ms < now_ms => { slot = i; break; }
                _ => { slot = i; } // overwrite last
            }
        }
        cache[slot] = Some(CacheEntry {
            hostname: String::from(hostname),
            ip,
            expires_ms: expires,
        });

        Ok(ip)
    } else {
        result.map(|(ip, _)| ip)
    }
}

/// Build a DNS query packet for an A record.
fn build_query(hostname: &str) -> Result<alloc::vec::Vec<u8>, DnsError> {
    if hostname.is_empty() || hostname.len() > 253 {
        return Err(DnsError::InvalidHostname);
    }

    let mut pkt = alloc::vec::Vec::with_capacity(64);

    // Header (12 bytes)
    // ID = 0x1234 (arbitrary, we only send one query at a time)
    pkt.extend_from_slice(&[0x12, 0x34]);
    // Flags: standard query, recursion desired
    pkt.extend_from_slice(&[0x01, 0x00]);
    // QDCOUNT = 1
    pkt.extend_from_slice(&[0x00, 0x01]);
    // ANCOUNT = 0
    pkt.extend_from_slice(&[0x00, 0x00]);
    // NSCOUNT = 0
    pkt.extend_from_slice(&[0x00, 0x00]);
    // ARCOUNT = 0
    pkt.extend_from_slice(&[0x00, 0x00]);

    // Question section: encode hostname as labels
    for label in hostname.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(DnsError::InvalidHostname);
        }
        pkt.push(label.len() as u8);
        pkt.extend_from_slice(label.as_bytes());
    }
    pkt.push(0x00); // root label

    // QTYPE = A (1)
    pkt.extend_from_slice(&[0x00, 0x01]);
    // QCLASS = IN (1)
    pkt.extend_from_slice(&[0x00, 0x01]);

    Ok(pkt)
}

/// Parse a DNS response and extract the first A record.
/// Returns (ip, ttl_seconds).
fn parse_response(data: &[u8], _hostname: &str) -> Result<(Ipv4Address, u32), DnsError> {
    if data.len() < 12 {
        return Err(DnsError::MalformedResponse);
    }

    // Check response flags
    let flags = u16::from_be_bytes([data[2], data[3]]);
    let qr = (flags >> 15) & 1;
    let rcode = flags & 0x0F;

    if qr != 1 {
        return Err(DnsError::MalformedResponse); // Not a response
    }
    if rcode != 0 {
        return Err(DnsError::NoAnswer); // Server error
    }

    let qdcount = u16::from_be_bytes([data[4], data[5]]);
    let ancount = u16::from_be_bytes([data[6], data[7]]);

    if ancount == 0 {
        return Err(DnsError::NoAnswer);
    }

    // Skip question section
    let mut pos = 12;
    for _ in 0..qdcount {
        pos = skip_name(data, pos)?;
        pos += 4; // QTYPE + QCLASS
        if pos > data.len() {
            return Err(DnsError::MalformedResponse);
        }
    }

    // Parse answer section — find first A record
    for _ in 0..ancount {
        pos = skip_name(data, pos)?;

        if pos + 10 > data.len() {
            return Err(DnsError::MalformedResponse);
        }

        let rtype = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let _rclass = u16::from_be_bytes([data[pos + 2], data[pos + 3]]);
        let ttl = u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]]);
        let rdlength = u16::from_be_bytes([data[pos + 8], data[pos + 9]]) as usize;
        pos += 10;

        if pos + rdlength > data.len() {
            return Err(DnsError::MalformedResponse);
        }

        if rtype == 1 && rdlength == 4 {
            // A record
            let ip = Ipv4Address::new(data[pos], data[pos + 1], data[pos + 2], data[pos + 3]);
            return Ok((ip, ttl));
        }

        pos += rdlength;
    }

    Err(DnsError::NoAnswer)
}

/// Skip a DNS name at the given position, handling compression pointers.
fn skip_name(data: &[u8], mut pos: usize) -> Result<usize, DnsError> {
    loop {
        if pos >= data.len() {
            return Err(DnsError::MalformedResponse);
        }

        let len = data[pos];
        if len == 0 {
            // Root label — end of name
            return Ok(pos + 1);
        } else if (len & 0xC0) == 0xC0 {
            // Compression pointer (2 bytes)
            return Ok(pos + 2);
        } else {
            // Regular label
            pos += 1 + (len as usize);
        }
    }
}
