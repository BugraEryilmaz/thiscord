//! Per-packet, per-frame and per-callback cost of native media hot paths that
//! need no devices or network: RED parse/encode (every received/sent 20 ms voice
//! frame), the playback mixer callback, and the H.264 NAL scans run several times
//! per encoded/received video frame. Reports allocations and time per operation.
//!
//! cargo run -p thiscord-frontend --example media_hotpath_bench --features native-audio --profile ci --locked
// `black_box(&mut ..)` is deliberate: it stops the optimizer from hoisting pure
// scans of unchanged input out of the timing loops.
#![allow(clippy::unnecessary_mut_passed)]
#[cfg(all(not(target_arch = "wasm32"), feature = "native-audio"))]
mod counting {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicU64, Ordering};
    pub static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
    pub struct Counting;
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            unsafe { System.realloc(ptr, layout, size) }
        }
    }
    #[global_allocator]
    static GLOBAL: Counting = Counting;
}

#[cfg(all(not(target_arch = "wasm32"), feature = "native-audio"))]
fn main() {
    use counting::ALLOCATIONS;
    use std::{
        hint::black_box,
        sync::{Arc, atomic::Ordering},
        time::Instant,
    };
    use thiscord_frontend::audio::mixer::{Controls, FRAME, mixer};
    use thiscord_shared::rtp_red::{self as red, Block};

    // Inputs are passed as `black_box(&mut ..)` so the optimizer cannot hoist a
    // pure scan of unchanged bytes out of the timing loop.
    fn measure(name: &str, iterations: u64, mut op: impl FnMut()) {
        for _ in 0..iterations / 10 {
            op();
        }
        let allocations = ALLOCATIONS.load(Ordering::Relaxed);
        let start = Instant::now();
        for _ in 0..iterations {
            op();
        }
        let elapsed = start.elapsed();
        let allocations = ALLOCATIONS.load(Ordering::Relaxed) - allocations;
        println!(
            "{name:<44} {:>9.1} ns/op {:>6.2} allocations/op",
            elapsed.as_nanos() as f64 / iterations as f64,
            allocations as f64 / iterations as f64
        );
    }

    // A typical 32 kbit/s Opus frame is ~80 bytes; RED carries two prior frames.
    let frame = [0x78_u8; 80];
    let history = [
        Block {
            offset: 1920,
            payload: &frame,
        },
        Block {
            offset: 960,
            payload: &frame,
        },
    ];
    let mut packet = red::encode(&frame, &history).expect("valid RED packet");
    let payload = bytes::Bytes::from(packet.clone());
    measure("RED parse (2 history blocks)", 1_000_000, || {
        let parsed = red::parse(black_box(&mut packet)).unwrap();
        black_box(parsed.history.len());
    });
    measure("RED parse + receive slices (Bytes)", 1_000_000, || {
        let parsed = red::parse(black_box(&payload)).unwrap();
        black_box(payload.slice_ref(parsed.primary));
        for block in parsed.history {
            black_box(payload.slice_ref(block.payload));
        }
    });
    measure("RED encode (2 history blocks)", 1_000_000, || {
        black_box(red::encode(black_box(&frame), black_box(&history)));
    });
    // Per-frame quality accounting on the worker: the receive path's counts
    // plus one stage timing, as recorded for every decoded 20 ms frame.
    {
        use std::time::Duration;
        use thiscord_frontend::audio::quality::{Count, Metrics, Stage};
        let metrics = Metrics::default();
        measure("quality metrics: 5 counts + 1 timing", 1_000_000, || {
            for count in [
                Count::ReceivedPackets,
                Count::DecodedSamples,
                Count::PlcSamples,
                Count::FecAttempts,
                Count::PcmOverflowSamples,
            ] {
                metrics.add(count, black_box(1));
            }
            metrics.duration(Stage::Decode, black_box(Duration::from_micros(40)));
        });
    }
    // ARENA-START (absent before the pooled send arena)
    {
        use thiscord_frontend::audio::arena::PacketArena;
        let opus = [0x78_u8; 100];
        measure("Opus payload: Bytes::copy_from_slice", 1_000_000, || {
            black_box(bytes::Bytes::copy_from_slice(black_box(&opus)));
        });
        let mut arena = PacketArena::default();
        // Hold the last two packets, as RED history does.
        let mut held = std::collections::VecDeque::new();
        measure("Opus payload: pooled arena", 1_000_000, || {
            held.push_back(arena.copy(black_box(&opus)));
            if held.len() > 2 {
                held.pop_front();
            }
        });
        measure("RED encode into pooled arena", 1_000_000, || {
            let size = red::encoded_len(&frame, black_box(&history)).unwrap();
            held.push_back(arena.write(size, |buffer| {
                red::encode_with(&frame, &history, |bytes| buffer.extend_from_slice(bytes));
            }));
            if held.len() > 2 {
                held.pop_front();
            }
        });
    }
    // ARENA-END

    // Encoded screen frames: a 25 KB delta (about 12 Mbit/s at 60 fps) and a
    // 300 KB IDR. Slice bytes avoid 0/1 so only real start codes match.
    let mut delta: Vec<u8> = vec![0, 0, 0, 1, 0x09, 0xf0, 0, 0, 1, 0x41];
    delta.extend((0..25_000).map(|i| 2 + (i % 250) as u8));
    let mut idr: Vec<u8> = vec![
        0, 0, 0, 1, 0x67, 66, 0xc0, 0x1f, 0, 0, 1, 0x68, 0xce, 0, 0, 1, 0x65,
    ];
    idr.extend((0..300_000).map(|i| 2 + (i % 250) as u8));
    use thiscord_shared::h264;
    measure("H.264 nal_units count, 25 KB delta frame", 20_000, || {
        black_box(h264::nal_units(black_box(&mut delta)).count());
    });
    measure("H.264 recovery_frame, 25 KB delta frame", 20_000, || {
        black_box(h264::recovery_frame(black_box(&mut delta)));
    });
    measure("H.264 recovery_frame, 300 KB IDR frame", 2_000, || {
        black_box(h264::recovery_frame(black_box(&mut idr)));
    });
    let mut assembler = h264::Assembler::default();
    let mut sequence = 0u16;
    let mut timestamp = 0u32;
    let fragment = [0x41_u8; 1_100];
    measure("H.264 assemble 25 KB frame (23 packets)", 20_000, || {
        timestamp = timestamp.wrapping_add(1_500);
        for packet in 0..23 {
            sequence = sequence.wrapping_add(1);
            black_box(assembler.push(timestamp, sequence, packet == 22, &fragment));
        }
    });

    for talkers in [1usize, 8] {
        let (mut writers, mut mixer) = mixer(Arc::new(Controls::default()));
        for writer in writers.iter().take(talkers) {
            writer.control.active.store(true, Ordering::Release);
        }
        let input = [0.05_f32; FRAME];
        let mut output = [0.0_f32; FRAME * 2];
        measure(
            &format!("mixer 20 ms block, {talkers} talker(s) of 32 slots"),
            20_000,
            || {
                for writer in writers.iter_mut().take(talkers) {
                    writer.write(&input);
                }
                mixer.render(&mut output, 2);
                black_box(&output);
            },
        );
    }
}
#[cfg(not(all(not(target_arch = "wasm32"), feature = "native-audio")))]
fn main() {
    eprintln!("Enable --features native-audio on a native host");
}
