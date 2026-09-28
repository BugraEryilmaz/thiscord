#[cfg(all(not(target_arch = "wasm32"), feature = "native-audio"))]
fn main() {
    match thiscord_frontend::audio::devices() {
        Ok(devices) => println!(
            "Native audio devices: {} inputs, {} outputs",
            devices.iter().filter(|d| d.input).count(),
            devices.iter().filter(|d| !d.input).count()
        ),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
#[cfg(not(all(not(target_arch = "wasm32"), feature = "native-audio")))]
fn main() {
    eprintln!("Enable --features native-audio on a native host");
}
