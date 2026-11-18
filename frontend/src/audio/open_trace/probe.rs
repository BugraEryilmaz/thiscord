use super::*;
use std::{collections::hash_map::RandomState, hash::BuildHasher, sync::OnceLock};
use windows::Win32::{
    Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eConsole, eRender},
    System::Com::{
        CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
        CoUninitialize,
    },
};

struct Slot(&'static AtomicBool);
impl Slot {
    fn acquire(busy: &'static AtomicBool) -> Option<Self> {
        busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self(busy))
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(super) fn schedule(open_id: u64, input: bool, compare: bool, at: Instant) {
    static BUSY: AtomicBool = AtomicBool::new(false);
    if !diagnostics::collecting() {
        return;
    }
    let direction = if input { "input" } else { "output" };
    let event = if compare {
        "device_format_probe"
    } else {
        "device_default_lookup"
    };
    let skipped = |reason: &str| {
        diagnostics::event_with(event, || {
            json!({
                "open_id":open_id, "direction":direction, "skipped":reason,
                "ms_since_failure":at.elapsed().as_millis()
            })
        })
    };
    let Some(slot) = Slot::acquire(&BUSY) else {
        skipped("probe_busy");
        return;
    };
    let worker = thread::Builder::new().name("thiscord-default-probe".into()).spawn(move || {
        let _slot = slot;
        let mut details = json!({
            "open_id":open_id,"direction":direction,"start_ms_since_failure":at.elapsed().as_millis()
        });
        if let Err((operation,e)) = inspect(input,compare,&mut details) {
            details["failed_operation"] = operation.into();
            details["hresult"] = json!(format!("{:#010X}",e.code().0 as u32));
        }
        if details.get("thread").is_none() { details["thread"] = thread_context(); }
        details["ms_since_failure"] = json!(at.elapsed().as_millis());
        diagnostics::event(event,details);
    });
    if worker.is_err() {
        skipped("worker_unavailable");
    }
}

fn alias(id: &str) -> String {
    static KEY: OnceLock<RandomState> = OnceLock::new();
    format!("{:016x}", KEY.get_or_init(RandomState::new).hash_one(id))
}

fn inspect(
    input: bool,
    compare: bool,
    v: &mut Value,
) -> Result<(), (&'static str, windows::core::Error)> {
    // Dedicated fresh worker, in the same apartment type CPAL gives the real
    // opening threads, so a by-ID result here is comparable with theirs.
    // All interfaces drop before its COM reference.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() }
        .map_err(|e| ("initialize", e))?;
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() }
        }
    }
    let _apartment = Apartment;
    v["thread"] = thread_context();
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(|e| ("enumerator", e))?;
    let lookup = || {
        let endpoint = unsafe {
            enumerator.GetDefaultAudioEndpoint(if input { eCapture } else { eRender }, eConsole)
        }
        .map_err(|e| ("default_endpoint", e))?;
        let raw = unsafe { endpoint.GetId() }.map_err(|e| ("get_id", e))?;
        let id = unsafe { raw.to_string() };
        unsafe { CoTaskMemFree(Some(raw.0.cast())) };
        id.map_err(|e| ("decode_id", e.into()))
    };
    let before = lookup()?;
    v["endpoint"] = alias(&before).into();
    if compare {
        let host = cpal::default_host();
        let device = host.device_by_id(&cpal::DeviceId::new(host.id(), before.clone()));
        v["thread"] = thread_context();
        if let Some(device) = device {
            v["name"] = json!(
                device
                    .description()
                    .ok()
                    .map(|d| d.name().chars().take(80).collect::<String>())
            );
            let queries = format::Queries::run(&device, input);
            v["queries"] = queries.details();
            v["compatible"] = queries.selected(input).is_some().into();
        } else {
            v["device_missing"] = true.into();
        }
        let after = lookup()?;
        v["endpoint_after"] = alias(&after).into();
        v["changed"] = (before != after).into();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_one_probe_until_worker_releases_slot() {
        static BUSY: AtomicBool = AtomicBool::new(false);
        let worker = Slot::acquire(&BUSY).unwrap();
        assert!(Slot::acquire(&BUSY).is_none());
        drop(worker);
        assert!(Slot::acquire(&BUSY).is_some());
    }
}
