//! Allocation budgets for native media hot paths. A counting allocator records
//! allocations made by the calling thread only, so parallel tests cannot skew a
//! measurement. Timings live in `examples/media_hotpath_bench.rs`.
#![cfg(all(not(target_arch = "wasm32"), feature = "screen-share"))]

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    hint::black_box,
};

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}
struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

/// Mean allocations per call of `op` on this thread, after a warm-up.
fn per_op(iterations: u64, mut op: impl FnMut()) -> f64 {
    for _ in 0..iterations / 10 + 1 {
        op();
    }
    let before = ALLOCATIONS.with(Cell::get);
    for _ in 0..iterations {
        op();
    }
    (ALLOCATIONS.with(Cell::get) - before) as f64 / iterations as f64
}

use thiscord_shared::{h264, rtp_red as red};

fn red_input() -> ([u8; 80], [red::Block<'static>; 2]) {
    static FRAME: [u8; 80] = [0x78; 80];
    (
        FRAME,
        [
            red::Block {
                offset: 1920,
                payload: &FRAME,
            },
            red::Block {
                offset: 960,
                payload: &FRAME,
            },
        ],
    )
}

#[test]
fn red_receive_parses_and_slices_without_allocating() {
    let (frame, history) = red_input();
    let packet = bytes::Bytes::from(red::encode(&frame, &history).unwrap());
    let cost = per_op(10_000, || {
        let parsed = red::parse(black_box(&packet)).unwrap();
        black_box(packet.slice_ref(parsed.primary));
        for block in parsed.history {
            black_box(packet.slice_ref(block.payload));
        }
    });
    assert_eq!(cost, 0.0);
}

#[test]
fn voice_send_payloads_come_from_the_pooled_arena() {
    use thiscord_frontend::audio::arena::PacketArena;
    let (frame, history) = red_input();
    let mut arena = PacketArena::default();
    // RED keeps the last two frames alive, as the sender's history does.
    let mut held = std::collections::VecDeque::new();
    let mut hold = |packet| {
        held.push_back(packet);
        if held.len() > 2 {
            held.pop_front();
        }
    };
    let opus = per_op(10_000, || hold(arena.copy(black_box(&frame))));
    assert!(opus < 0.1, "{opus} allocations per Opus payload");
    let encoded = per_op(10_000, || {
        let size = red::encoded_len(&frame, &history).unwrap();
        hold(arena.write(size, |buffer| {
            red::encode_with(&frame, &history, |bytes| buffer.extend_from_slice(bytes));
        }));
    });
    assert!(encoded < 0.1, "{encoded} allocations per RED packet");
}

#[test]
fn h264_scans_and_assembly_stay_within_budget() {
    let mut delta = vec![0, 0, 0, 1, 0x09, 0xf0, 0, 0, 1, 0x41];
    delta.extend((0..25_000).map(|i| 2 + (i % 250) as u8));
    assert_eq!(
        per_op(1_000, || assert!(!h264::recovery_frame(black_box(&delta)))),
        0.0
    );
    assert_eq!(
        per_op(1_000, || h264::front_units(black_box(&delta), |nal| {
            black_box(nal);
        })),
        0.0
    );
    let mut assembler = h264::Assembler::default();
    let (mut sequence, mut timestamp) = (0u16, 0u32);
    let fragment = [0x41_u8; 1_100];
    let cost = per_op(1_000, || {
        timestamp = timestamp.wrapping_add(1_500);
        for packet in 0..23 {
            sequence = sequence.wrapping_add(1);
            black_box(assembler.push(timestamp, sequence, packet == 22, &fragment));
        }
    });
    // The finished frame leaves with its buffer; one presized replacement.
    assert!(cost <= 1.0, "{cost} allocations per assembled frame");
}

#[test]
fn packetizing_a_frame_does_not_allocate_per_packet() {
    let mut frame = vec![
        0, 0, 0, 1, 0x67, 66, 0xc0, 0x1f, 0, 0, 1, 0x68, 0xce, 0, 0, 1, 0x65,
    ];
    frame.extend((0..300_000).map(|i| 2 + (i % 250) as u8));
    let mut sequence = 0;
    let packets = thiscord_frontend::screen::packetize(frame.clone(), &mut sequence, 0).unwrap();
    assert!(packets.len() > 250);
    // Minus the one allocation for the input `Vec` the caller hands over.
    let cost = per_op(100, || {
        black_box(thiscord_frontend::screen::packetize(frame.clone(), &mut sequence, 0).unwrap());
    }) - 1.0;
    // A fixed handful per frame (packet list, NAL slices, one fragment buffer,
    // shared-frame promotion), independent of the packet count; the webrtc-rs
    // payloader allocated once per FU-A fragment.
    assert!(
        cost <= 8.0,
        "{cost} allocations for {} packets",
        packets.len()
    );
}

#[test]
fn mixer_callbacks_do_not_allocate() {
    use std::sync::{Arc, atomic::Ordering};
    use thiscord_frontend::audio::mixer::{Controls, FRAME, mixer};
    let (mut writers, mut mixer) = mixer(Arc::new(Controls::default()));
    writers[0].control.active.store(true, Ordering::Release);
    let input = [0.05_f32; FRAME];
    let mut output = [0.0_f32; FRAME * 2];
    let cost = per_op(1_000, || {
        writers[0].write(&input);
        mixer.render(&mut output, 2);
        black_box(&output);
    });
    assert_eq!(cost, 0.0);
}
