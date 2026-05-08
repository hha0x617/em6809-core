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
