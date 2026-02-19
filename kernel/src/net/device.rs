/// smoltcp PHY device adapter for virtio-net.
///
/// This bridges the gap between our virtio-net driver (which sends/receives
/// raw Ethernet frames) and smoltcp (which expects a `Device` trait impl).
use alloc::vec::Vec;
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;

use crate::drivers::virtio::net::VIRTIO_NET;

/// Adapter that implements smoltcp's Device trait using virtio-net.
pub struct SmoltcpDevice;

impl SmoltcpDevice {
    pub fn new() -> Self {
        Self
    }

    pub fn mac(&self) -> Option<[u8; 6]> {
        VIRTIO_NET.lock().as_ref().map(|nic| nic.mac())
    }
}

impl Device for SmoltcpDevice {
    type RxToken<'a> = RxToken;
    type TxToken<'a> = TxToken;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let mut nic = VIRTIO_NET.lock();
        let nic = nic.as_mut()?;

        let frame = nic.receive()?;
        Some((RxToken { frame }, TxToken))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        // Always ready to transmit (queue full handled in the driver)
        Some(TxToken)
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = 1514;
        caps.max_burst_size = Some(1);
        caps
    }
}

/// Receive token — holds a received Ethernet frame.
pub struct RxToken {
    frame: Vec<u8>,
}

impl phy::RxToken for RxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.frame)
    }
}

/// Transmit token — provides a buffer to write an outgoing frame.
pub struct TxToken;

impl phy::TxToken for TxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = alloc::vec![0u8; len];
        let result = f(&mut buf);

        // Send the frame through virtio-net
        let mut nic = VIRTIO_NET.lock();
        if let Some(nic) = nic.as_mut() {
            let _ = nic.transmit(&buf);
        }

        result
    }
}
