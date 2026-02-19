/// Virtio device drivers for QEMU/KVM.
///
/// Virtio is the standard paravirtualized I/O framework. QEMU exposes
/// virtio-net (network) and optionally virtio-blk (block device) via PCI.
///
/// We implement virtio-net here as the path to network connectivity,
/// which is required to reach the Claude API.
pub mod net;
pub mod virtqueue;
