//! # `timer` — minimal periodic timer device
//!
//! A small memory-mapped countdown timer that the GUI and integration
//! tests use to drive periodic interrupts.  It implements
//! [`crate::io::Device`] so it plugs straight into [`crate::io::IoBus`].
//!
//! ## Register layout
//!
//! Five bytes starting at `base`:
//!
//! | Offset | Direction | Field |
//! |---:|---|---|
//! | `+0` | R/W | CTRL/STATUS — bit0 `RUN`, bit1 `IRQ_EN`, bit2 `FIRQ` (route as FIRQ instead of IRQ), bit3 `PENDING` (R), bit4 write-1-to-clear `PENDING` |
//! | `+1..+2` | R/W | `RELOAD` — 16-bit period in instruction ticks |
//! | `+3..+4` | R/W | `COUNTER` — current down-counter |
//!
//! When `RUN=1` and `IRQ_EN=1`, the device asserts an IRQ (or FIRQ if
//! bit 2 is set) every `RELOAD` ticks.  The handler clears `PENDING`
//! by writing 1 to bit 4.
//!
//! ## Provided type
//!
//! - [`TimerDev`] — the device itself.  Methods:
//!   - [`TimerDev::new`] (`base`) — fresh, stopped.
//!   - [`TimerDev::set_reload`] / [`TimerDev::start`] / [`TimerDev::stop`]
//!     — programmatic control without going through register writes
//!     (useful for integration tests).
//!   - [`TimerDev::set_irq_enable`] / [`TimerDev::set_firq`] — IRQ/FIRQ
//!     wiring without touching CTRL.
//!   - [`TimerDev::get_state`] -> `(run, irq_en, firq, pending)` —
//!     UI snapshot.
//!   - [`TimerDev::get_info`] -> `(reload, counter)` — UI snapshot.
//!
//! ## Typical usage
//!
//! ```no_run
//! use em6809_core::bus::Memory;
//! use em6809_core::io::IoBus;
//! use em6809_core::timer::TimerDev;
//!
//! let mut bus = IoBus::new(Memory::new());
//! let mut t = TimerDev::new(0xFF20);
//! t.set_reload(10_000);
//! t.set_irq_enable(true);
//! t.start();
//! bus.add_device(t);
//! ```
//!
//! Or, simpler, let `IoBus::ensure_timer` handle install/teardown
//! from a config flag.

use crate::io::Device;
use std::any::Any;

// Minimal periodic Timer device
// Registers (base..base+4):
//  +0 CTRL/STATUS (R/W): bit0 RUN, bit1 IRQ_EN, bit2 FIRQ, bit3 PENDING (R), bit4: write 1 to clear PENDING
//  +1..+2 RELOAD (R/W): 16-bit period in instruction ticks
//  +3..+4 COUNTER (R/W): current down-counter
pub struct TimerDev {
    pub base: u16,
    ctrl: u8,
    reload: u16,
    counter: u16,
    pending: bool,
}

impl TimerDev {
    pub fn new(base: u16) -> Self {
        Self {
            base,
            ctrl: 0,
            reload: 0,
            counter: 0,
            pending: false,
        }
    }
    fn running(&self) -> bool {
        (self.ctrl & 0x01) != 0
    }
    fn irq_en(&self) -> bool {
        (self.ctrl & 0x02) != 0
    }
    fn use_firq(&self) -> bool {
        (self.ctrl & 0x04) != 0
    }
    fn tick(&mut self) {
        if !self.running() {
            return;
        }
        if self.counter > 0 {
            self.counter = self.counter.wrapping_sub(1);
        }
        if self.counter == 0 {
            self.pending = true;
            self.counter = self.reload;
        }
    }
    pub fn set_reload(&mut self, val: u16) {
        self.reload = val;
        if self.counter == 0 {
            self.counter = val;
        }
    }
    pub fn start(&mut self) {
        self.ctrl |= 0x01;
        if self.counter == 0 {
            self.counter = self.reload;
        }
    }
    pub fn stop(&mut self) {
        self.ctrl &= !0x01;
    }
    pub fn set_irq_enable(&mut self, on: bool) {
        if on {
            self.ctrl |= 0x02;
        } else {
            self.ctrl &= !0x02;
        }
    }
    pub fn set_firq(&mut self, on: bool) {
        if on {
            self.ctrl |= 0x04;
        } else {
            self.ctrl &= !0x04;
        }
    }
    pub fn get_state(&self) -> (bool, bool, bool, bool) {
        (self.running(), self.irq_en(), self.use_firq(), self.pending)
    }
    pub fn get_info(&self) -> (u16, u16) {
        (self.base, self.reload)
    }
}

impl Device for TimerDev {
    fn contains(&self, addr: u16) -> bool {
        addr.wrapping_sub(self.base) < 5
    }
    fn read8(&mut self, addr: u16) -> u8 {
        match addr.wrapping_sub(self.base) {
            0 => {
                let mut s = self.ctrl;
                if self.pending {
                    s |= 0x08;
                }
                s
            }
            1 => (self.reload >> 8) as u8,
            2 => (self.reload & 0xFF) as u8,
            3 => (self.counter >> 8) as u8,
            4 => (self.counter & 0xFF) as u8,
            _ => 0xFF,
        }
    }
    fn write8(&mut self, addr: u16, data: u8) {
        match addr.wrapping_sub(self.base) {
            0 => {
                if (data & 0x10) != 0 {
                    self.pending = false;
                }
                let was_running = self.running();
                self.ctrl = (self.ctrl & 0x08) | (data & 0x07);
                if !was_running && self.running() && self.counter == 0 {
                    self.counter = self.reload;
                }
            }
            1 => {
                self.reload = (self.reload & 0x00FF) | ((data as u16) << 8);
            }
            2 => {
                self.reload = (self.reload & 0xFF00) | (data as u16);
            }
            3 => {
                self.counter = (self.counter & 0x00FF) | ((data as u16) << 8);
            }
            4 => {
                self.counter = (self.counter & 0xFF00) | (data as u16);
            }
            _ => {}
        }
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn irq_lines(&mut self) -> (bool, bool, bool) {
        self.tick();
        let on = self.pending && self.irq_en();
        if self.use_firq() {
            (false, on, false)
        } else {
            (false, false, on)
        }
    }
}
