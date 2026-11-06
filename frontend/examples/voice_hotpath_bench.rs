//! Per-packet and per-callback cost of the native voice hot paths that do not
//! need devices or a network: RED parse/encode (every received/sent 20 ms
//! frame) and the playback mixer callback with one talker among 32 slots.
//! Reports heap allocations per operation and mean time per operation.
//!
//! cargo run -p thiscord-frontend --example voice_hotpath_bench --features native-audio --profile ci --locked
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
    let packet = red::encode(&frame, &history).expect("valid RED packet");
    let payload = bytes::Bytes::from(packet.clone());
    measure("RED parse (2 history blocks)", 1_000_000, || {
        let parsed = red::parse(black_box(&packet)).unwrap();
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
