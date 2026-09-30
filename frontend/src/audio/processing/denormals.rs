//! Avoid slow hardware assists on inaudibly small recursive DSP values.
//! Floating point rounding/exceptions are unchanged. Restore the caller's state.
use std::{marker::PhantomData, rc::Rc};

pub(super) struct Guard {
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    previous: u64,
    // FP control registers are thread-local; a guard must never move threads.
    _thread: PhantomData<Rc<()>>,
}
impl Guard {
    pub fn new() -> Self {
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        let previous = read();
        #[cfg(target_arch = "x86_64")]
        write(previous | (1 << 15) | (1 << 6)); // MXCSR: FTZ + DAZ
        #[cfg(target_arch = "aarch64")]
        write(previous | (1 << 24)); // FPCR: flush subnormal inputs/results
        Self {
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            previous,
            _thread: PhantomData,
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        write(self.previous);
    }
}
#[cfg(target_arch = "x86_64")]
fn read() -> u64 {
    let mut value = 0_u32;
    // SAFETY: SSE2 is baseline on x86_64; the pointer names a writable u32.
    unsafe {
        std::arch::asm!("stmxcsr [{}]", in(reg) &mut value, options(nostack, preserves_flags));
    }
    u64::from(value)
}
#[cfg(target_arch = "x86_64")]
fn write(value: u64) {
    let value = value as u32;
    // SAFETY: this is a previously read MXCSR with only documented FTZ/DAZ bits
    // added. Reserved bits, exception masks and rounding mode are preserved.
    unsafe {
        std::arch::asm!("ldmxcsr [{}]", in(reg) &value, options(nostack, preserves_flags));
    }
}
#[cfg(target_arch = "aarch64")]
fn read() -> u64 {
    let value: u64;
    // SAFETY: FPCR is accessible at user privilege on supported ARM64 desktops.
    unsafe {
        std::arch::asm!("mrs {}, fpcr", out(reg) value, options(nostack, preserves_flags));
    }
    value
}
#[cfg(target_arch = "aarch64")]
fn write(value: u64) {
    // SAFETY: preserve all control bits except documented FZ; restore on drop.
    unsafe {
        std::arch::asm!("msr fpcr, {}", in(reg) value, options(nostack, preserves_flags));
    }
}
#[cfg(all(test, any(target_arch = "x86_64", target_arch = "aarch64")))]
mod tests {
    use super::*;
    #[test]
    fn scoped_float_mode_restores_callers_control_register() {
        let before = read();
        {
            let _guard = Guard::new();
            #[cfg(target_arch = "x86_64")]
            assert_eq!(read(), before | (1 << 15) | (1 << 6));
            #[cfg(target_arch = "aarch64")]
            assert_eq!(read(), before | (1 << 24));
            {
                let _nested = Guard::new();
            }
        }
        assert_eq!(read(), before);
    }
}
