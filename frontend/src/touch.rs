//! Long-press recognition so touch screens reach right-click menus and hints.
//!
//! The browser layer feeds touch events and a timer into this state machine;
//! it never sees DOM types, so the timing rules are unit-tested natively.

/// How long a finger must rest before the menu opens.
pub const HOLD_MS: u32 = 500;
/// Movement beyond this distance is a scroll or drag, not a press.
pub const SLOP_PX: f64 = 10.0;
/// Clicks synthesized from the released press are swallowed for this long.
pub const CLICK_GUARD_MS: f64 = 800.0;

#[derive(Debug, Default)]
pub struct LongPress {
    origin: Option<(f64, f64)>,
    generation: u64,
    fired: bool,
    guard_until: f64,
}

impl LongPress {
    /// A new touch sequence begins. Only single-finger touches can become a
    /// long press; returns the generation the hold timer must present.
    pub fn start(&mut self, x: f64, y: f64, touches: u32) -> Option<u64> {
        self.generation = self.generation.wrapping_add(1);
        self.fired = false;
        self.origin = (touches == 1).then_some((x, y));
        self.origin.map(|_| self.generation)
    }

    pub fn moved(&mut self, x: f64, y: f64) {
        if self
            .origin
            .is_some_and(|(sx, sy)| (x - sx).hypot(y - sy) > SLOP_PX)
        {
            self.origin = None;
        }
    }

    /// The finger lifted or the browser cancelled the touch.
    pub fn end(&mut self, now: f64) {
        self.origin = None;
        if self.fired {
            self.fired = false;
            self.guard_until = now + CLICK_GUARD_MS;
        }
    }

    /// Another recognizer (such as a native long-press `contextmenu`) handled it.
    pub fn cancel(&mut self) {
        self.origin = None;
        self.generation = self.generation.wrapping_add(1);
    }

    /// The hold timer elapsed. Returns where the press happened when the same
    /// touch is still down and still.
    pub fn elapsed(&mut self, generation: u64) -> Option<(f64, f64)> {
        if generation != self.generation {
            return None;
        }
        let origin = self.origin.take()?;
        self.fired = true;
        Some(origin)
    }

    /// True while the current touch already opened something, and briefly
    /// after release, so the follow-up click or native menu does not also run.
    pub fn suppresses(&self, now: f64) -> bool {
        self.fired || now < self.guard_until
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn still_single_touch_fires_once_and_guards_the_release_click() {
        let mut press = LongPress::default();
        let generation = press.start(10.0, 20.0, 1).unwrap();
        press.moved(14.0, 23.0);
        assert!(!press.suppresses(0.0));
        assert_eq!(press.elapsed(generation), Some((10.0, 20.0)));
        assert_eq!(press.elapsed(generation), None);
        assert!(press.suppresses(600.0));
        press.end(1000.0);
        assert!(press.suppresses(1000.0 + CLICK_GUARD_MS - 1.0));
        assert!(!press.suppresses(1000.0 + CLICK_GUARD_MS));
        // The next touch starts clean.
        press.start(0.0, 0.0, 1);
        assert!(!press.suppresses(2000.0 + CLICK_GUARD_MS));
    }

    #[test]
    fn movement_release_multitouch_and_stale_timers_never_fire() {
        let mut press = LongPress::default();
        let generation = press.start(0.0, 0.0, 1).unwrap();
        press.moved(SLOP_PX + 1.0, 0.0);
        assert_eq!(press.elapsed(generation), None);

        let generation = press.start(0.0, 0.0, 1).unwrap();
        press.end(100.0);
        assert_eq!(press.elapsed(generation), None);
        assert!(!press.suppresses(100.0));

        assert_eq!(press.start(0.0, 0.0, 2), None);

        let stale = press.start(0.0, 0.0, 1).unwrap();
        let current = press.start(5.0, 5.0, 1).unwrap();
        assert_eq!(press.elapsed(stale), None);
        assert_eq!(press.elapsed(current), Some((5.0, 5.0)));

        let generation = press.start(0.0, 0.0, 1).unwrap();
        press.cancel();
        assert_eq!(press.elapsed(generation), None);
    }
}
