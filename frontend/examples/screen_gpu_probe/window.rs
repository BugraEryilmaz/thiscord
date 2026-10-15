//! Dedicated synthetic window: its HWND is the only capture source in the probe.
use super::scene::Scene;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::{
            Direct3D::*,
            Direct3D11::*,
            Dxgi::{Common::*, *},
        },
        UI::{HiDpi::*, WindowsAndMessaging::*},
    },
    core::{Interface, w},
};

pub struct Source {
    pub id: u64,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Result<(), String>>>,
}
impl Source {
    pub fn new() -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let result = unsafe { render(done, &tx) }.map_err(|e| format!("Synthetic window: {e}"));
            if let Err(e) = &result {
                let _ = tx.send(Err(e.clone()));
            }
            result
        });
        let mut source = Self {
            id: 0,
            stop,
            thread: Some(thread),
        };
        source.id = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .map_err(|e| e.to_string())??;
        Ok(source)
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}
unsafe fn render(
    stop: Arc<AtomicBool>,
    tx: &std::sync::mpsc::SyncSender<Result<u64, String>>,
) -> windows::core::Result<()> {
    unsafe {
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let class = w!("ThiscordSynthetic4KProbe");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        // Does not change display mode. Window capture preserves its full 4K
        // client area even on the attached 3440x1440 monitor.
        let hwnd = CreateWindowExW(
            WS_EX_NOACTIVATE,
            class,
            w!("Thiscord synthetic 3840x2160 capture probe"),
            WS_POPUP | WS_VISIBLE,
            -3584,
            -1904,
            3840,
            2160,
            None,
            None,
            None,
            None,
        )?;
        let result = (|| -> windows::core::Result<()> {
            let (mut device, mut context) = (None, None);
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device = device.unwrap();
            let context = context.unwrap();
            let adapter = device.cast::<IDXGIDevice>()?.GetAdapter()?;
            let factory: IDXGIFactory2 = adapter.GetParent()?;
            let swap = factory.CreateSwapChainForHwnd(
                &device,
                hwnd,
                &DXGI_SWAP_CHAIN_DESC1 {
                    Width: 3840,
                    Height: 2160,
                    Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                    BufferCount: 2,
                    SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                    ..Default::default()
                },
                None,
                None,
            )?;
            swap.cast::<IDXGISwapChain3>()?
                .SetColorSpace1(DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709)?;
            let scene = Scene::new(&device, 3840, 2160)?;
            let _ = tx.send(Ok(hwnd.0 as u64));
            let mut frame = 0;
            while !stop.load(Ordering::Acquire) && IsWindow(Some(hwnd)).as_bool() {
                let mut message = MSG::default();
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                scene.draw(&context, frame)?;
                let back: ID3D11Texture2D = swap.GetBuffer(0)?;
                context.CopyResource(&back, &scene.texture);
                swap.Present(1, DXGI_PRESENT(0)).ok()?;
                frame += 1;
            }
            Ok(())
        })();
        let _ = DestroyWindow(hwnd);
        let _ = UnregisterClassW(class, None);
        result
    }
}
