//! Actual WGC -> GPU -> H.264 -> production pacer -> encrypted loopback WebRTC
//! -> production receive queue. Captures ONLY a dedicated synthetic 4K window.
#[cfg(windows)]
#[path = "screen_gpu_probe/browser.rs"]
mod browser;
#[cfg(windows)]
#[path = "screen_gpu_probe/pipeline.rs"]
mod pipeline;
#[cfg(windows)]
#[path = "screen_gpu_probe/scene.rs"]
mod scene;
#[cfg(windows)]
#[path = "screen_gpu_probe/window.rs"]
mod window;

#[cfg(windows)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    pipeline::run().await
}
#[cfg(not(windows))]
fn main() {
    println!("The WGC pipeline probe requires Windows");
}
