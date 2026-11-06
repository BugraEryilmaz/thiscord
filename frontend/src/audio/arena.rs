//! Pooled storage for outgoing encoded voice payloads (Opus, RED). Each packet
//! is a frozen slice of a shared block, so a 50 packet/s stream allocates one
//! block per `BLOCK` bytes instead of one buffer per packet. A block is reused
//! in place once every packet cut from it has been dropped.
use bytes::{Bytes, BytesMut};

/// Holds dozens of typical (~100 byte) Opus/RED packets.
const BLOCK: usize = 8 * 1024;

#[derive(Default)]
pub struct PacketArena {
    buffer: BytesMut,
}
impl PacketArena {
    /// Fills exactly `size` bytes through `write` and returns them.
    pub fn write(&mut self, size: usize, write: impl FnOnce(&mut BytesMut)) -> Bytes {
        if self.buffer.capacity() < size {
            // Reclaims the existing block when no packet still references it.
            self.buffer.reserve(BLOCK.max(size));
        }
        write(&mut self.buffer);
        debug_assert_eq!(self.buffer.len(), size);
        self.buffer.split().freeze()
    }
    pub fn copy(&mut self, data: &[u8]) -> Bytes {
        self.write(data.len(), |buffer| buffer.extend_from_slice(data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packets_are_independent_and_blocks_are_shared_then_reused() {
        let mut arena = PacketArena::default();
        let first = arena.copy(&[1; 100]);
        let second = arena.copy(&[2; 100]);
        assert_eq!(&first[..], &[1; 100]);
        assert_eq!(&second[..], &[2; 100]);
        // Both packets live in one block.
        assert_eq!(first.as_ptr().wrapping_add(100), second.as_ptr());
        let start = first.as_ptr();
        drop((first, second));
        // Fill the block; once every packet is gone it is reclaimed in place.
        let mut packets = Vec::new();
        while arena.buffer.capacity() >= 100 {
            packets.push(arena.copy(&[3; 100]));
        }
        drop(packets);
        let reused = arena.copy(&[4; 100]);
        assert_eq!(reused.as_ptr(), start);
        assert_eq!(&reused[..], &[4; 100]);
    }
}
