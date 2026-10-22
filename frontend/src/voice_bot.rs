fn main() {
    // No credentials on argv, environment, disk or logs. stdout is private IPC.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(thiscord_frontend::bot::run_stdio());
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    if result.is_err() {
        std::process::exit(1);
    }
}
