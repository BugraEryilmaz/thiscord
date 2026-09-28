#[cfg(all(not(target_arch = "wasm32"), feature = "native-audio"))]
fn main() {
    use std::{
        sync::{Arc, atomic::Ordering},
        time::Instant,
    };
    use thiscord_frontend::audio::mixer::{Controls, FRAME, mixer};
    let (mut writers, mut mixer) = mixer(Arc::new(Controls::default()));
    for writer in writers.iter().take(8) {
        writer.control.active.store(true, Ordering::Release);
    }
    let input = [0.05_f32; FRAME];
    let mut output = [0.0_f32; FRAME * 2];
    let start = Instant::now();
    for _ in 0..1000 {
        for writer in writers.iter_mut().take(8) {
            writer.write(&input);
        }
        mixer.render(&mut output, 2);
        std::hint::black_box(&output);
    }
    println!(
        "8-stream mixer: {:.3} ms per 20 ms audio block (1000 blocks, excludes codecs/devices/network)",
        start.elapsed().as_secs_f64()
    );
}
#[cfg(not(all(not(target_arch = "wasm32"), feature = "native-audio")))]
fn main() {
    eprintln!("Enable --features native-audio on a native host");
}
