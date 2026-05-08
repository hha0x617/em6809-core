#![allow(clippy::needless_range_loop)]
use crate::bus::Bus;

const PAGE_SHIFT: usize = 12; // 4KB pages
const PAGE_SIZE: usize = 1 << PAGE_SHIFT;
const NUM_LOG_PAGES: usize = 16; // 64KB / 4KB
const NUM_TASKS: usize = 8; // Simplified: 8 tasks

pub struct Mc6829 {
    phys: Vec<u8>,                               // physical memory
    pub task: u8,                                // current task (0..7)
    map_user: [[u16; NUM_LOG_PAGES]; NUM_TASKS], // user map
    map_sys: [[u16; NUM_LOG_PAGES]; NUM_TASKS],  // system map
    attr_user: [[u8; NUM_LOG_PAGES]; NUM_TASKS], // per-page attributes: bit0=WPROT, bit1=RPROT, bit2=NX
    attr_sys: [[u8; NUM_LOG_PAGES]; NUM_TASKS],
    sys_mode: bool,     // false=user, true=system
    prot_ctrl: u8, // bit0=WPROT_EN, bit1=IRQ on fault, bit2=RPROT_EN, bit3=NX_EN, bit4=FIRQ on fault
    fault_status: u8, // bit0=WPROT, bit1=RPROT, bit2=NX
    pub regs_base: u16, // register window base address (logical)
    regs_enable: bool,
    log_maps: bool,
}

impl Mc6829 {
    pub fn new(phys_bytes: usize, regs_base: u16) -> Self {
        let mut m = Self {
            phys: vec![0; phys_bytes],
            task: 0,
            map_user: [[0; NUM_LOG_PAGES]; NUM_TASKS],
            map_sys: [[0; NUM_LOG_PAGES]; NUM_TASKS],
            attr_user: [[0; NUM_LOG_PAGES]; NUM_TASKS],
            attr_sys: [[0; NUM_LOG_PAGES]; NUM_TASKS],
            sys_mode: false,
            prot_ctrl: 0,
            fault_status: 0,
            regs_base,
            regs_enable: true,
            log_maps: false,
        };
        // Identity map task 0 by default: page n -> frame n
        for i in 0..NUM_LOG_PAGES {
            m.map_user[0][i] = i as u16;
            m.map_sys[0][i] = i as u16;
        }
        m
    }

    pub fn identity_map_current(&mut self) {
        let t = self.task as usize;
        for i in 0..NUM_LOG_PAGES {
            self.map_user[t][i] = i as u16;
            self.map_sys[t][i] = i as u16;
        }
    }

    pub fn set_task(&mut self, t: u8) {
        self.task = (t as usize % NUM_TASKS) as u8;
    }
    pub fn set_log_maps(&mut self, on: bool) {
        self.log_maps = on;
    }
    pub fn set_map_entry(&mut self, page: usize, frame: u16) {
        let t = self.task as usize;
        if self.sys_mode {
            self.map_sys[t][page] = frame;
        } else {
            self.map_user[t][page] = frame;
        }
        if self.log_maps {
            let mode = if self.sys_mode { "sys" } else { "user" };
            println!(
                "[mmu] {} task{}: P{:X} -> F{:X}",
                mode,
                self.task,
                page & 0x0F,
                frame as usize & 0xFF
            );
        }
    }

    pub fn snapshot_current_map(&self) -> ([u16; NUM_LOG_PAGES], bool, u8) {
        let t = self.task as usize;
        let mut out = [0u16; NUM_LOG_PAGES];
        for i in 0..NUM_LOG_PAGES {
            let frame = if self.sys_mode {
                self.map_sys[t][i]
            } else {
                self.map_user[t][i]
            } & 0x00FF;
            let attr = if self.sys_mode {
                self.attr_sys[t][i]
            } else {
                self.attr_user[t][i]
            } as u16;
            out[i] = frame | (attr << 8);
        }
        (out, self.sys_mode, self.task)
    }

    pub fn snapshot_map_for(&self, sys_mode: bool) -> [u16; NUM_LOG_PAGES] {
        let t = self.task as usize;
        let mut out = [0u16; NUM_LOG_PAGES];
        for i in 0..NUM_LOG_PAGES {
            let frame = if sys_mode {
                self.map_sys[t][i]
            } else {
                self.map_user[t][i]
            } & 0x00FF;
            let attr = if sys_mode {
                self.attr_sys[t][i]
            } else {
                self.attr_user[t][i]
            } as u16;
            out[i] = frame | (attr << 8);
        }
        out
    }

    pub fn snapshot_maps(&self) -> ([u16; NUM_LOG_PAGES], [u16; NUM_LOG_PAGES], bool, u8) {
        let sys = self.snapshot_map_for(true);
        let user = self.snapshot_map_for(false);
        (sys, user, self.sys_mode, self.task)
    }

    fn in_regs(&self, addr: u16) -> bool {
        // MMU regs: 0x00..0x0F map, 0x10..0x13 ctrl/fault, 0x18..0x27 attr
        addr.wrapping_sub(self.regs_base) < 0x28
    }

    fn regs_read(&mut self, addr: u16) -> u8 {
        let off = addr.wrapping_sub(self.regs_base);
        match off as usize {
            // 0x00..0x0F: map registers for current task (8-bit frame index)
            0x00..=0x0F => {
                let page = off as usize;
                let frame = if self.sys_mode {
                    self.map_sys[self.task as usize][page]
                } else {
                    self.map_user[self.task as usize][page]
                };
                (frame & 0xFF) as u8
            }
            0x10 => self.task & 0x07, // task select
            0x11 => {
                if self.sys_mode {
                    1
                } else {
                    0
                }
            }
            0x12 => self.prot_ctrl,
            0x13 => {
                let s = self.fault_status;
                self.fault_status = 0;
                s
            }
            0x18..=0x27 => {
                let page = (off - 0x18) as usize;
                if self.sys_mode {
                    self.attr_sys[self.task as usize][page]
                } else {
                    self.attr_user[self.task as usize][page]
                }
            }
            _ => 0xFF,
        }
    }

    fn regs_write(&mut self, addr: u16, data: u8) {
        let off = addr.wrapping_sub(self.regs_base);
        match off as usize {
            0x00..=0x0F => {
                let page = off as usize;
                self.set_map_entry(page, data as u16);
            }
            0x10 => {
                self.task = data & 0x07;
            }
            0x11 => {
                self.sys_mode = (data & 0x01) != 0;
            }
            0x12 => {
                self.prot_ctrl = data;
            }
            0x18..=0x27 => {
                let page = (off - 0x18) as usize;
                if self.sys_mode {
                    self.attr_sys[self.task as usize][page] = data;
                } else {
                    self.attr_user[self.task as usize][page] = data;
                }
            }
            _ => {}
        }
    }

    fn translate(&self, laddr: u16) -> Option<usize> {
        let page = (laddr as usize) >> PAGE_SHIFT;
        let off = (laddr as usize) & (PAGE_SIZE - 1);
        let frame = if self.sys_mode {
            self.map_sys[self.task as usize][page]
        } else {
            self.map_user[self.task as usize][page]
        } as usize;
        let paddr = (frame << PAGE_SHIFT) | off;
        if paddr < self.phys.len() {
            Some(paddr)
        } else {
            None
        }
    }

    // Convenience for loaders
    pub fn store_logical_slice(&mut self, base: u16, bytes: &[u8]) {
        for (i, b) in bytes.iter().enumerate() {
            let addr = base.wrapping_add(i as u16);
            self.write8(addr, *b);
        }
    }

    pub fn store_physical_slice(&mut self, pbase: usize, bytes: &[u8]) {
        let end = pbase.saturating_add(bytes.len());
        if end <= self.phys.len() {
            self.phys[pbase..end].copy_from_slice(bytes);
        }
    }

    pub fn clear_physical(&mut self, value: u8) {
        self.phys.fill(value);
    }
}

impl Bus for Mc6829 {
    fn read8(&mut self, addr: u16) -> u8 {
        if self.regs_enable && self.in_regs(addr) {
            return self.regs_read(addr);
        }
        if let Some(paddr) = self.translate(addr) {
            // RPROT enforcement (data read)
            let page = (addr as usize) >> PAGE_SHIFT;
            let attr = if self.sys_mode {
                self.attr_sys[self.task as usize][page]
            } else {
                self.attr_user[self.task as usize][page]
            };
            let rprot_on = (self.prot_ctrl & 0x04) != 0 && (attr & 0x02) != 0;
            if rprot_on {
                self.fault_status |= 0x02;
                return 0xFF;
            }
            self.phys[paddr]
        } else {
            0xFF
        }
    }
    fn read8_fetch(&mut self, addr: u16) -> u8 {
        if self.regs_enable && self.in_regs(addr) {
            return self.regs_read(addr);
        }
        if let Some(paddr) = self.translate(addr) {
            // NX enforcement (instruction fetch)
            let page = (addr as usize) >> PAGE_SHIFT;
            let attr = if self.sys_mode {
                self.attr_sys[self.task as usize][page]
            } else {
                self.attr_user[self.task as usize][page]
            };
            let nx_on = (self.prot_ctrl & 0x08) != 0 && (attr & 0x04) != 0;
            if nx_on {
                self.fault_status |= 0x04;
                return 0x00;
            }
            self.phys[paddr]
        } else {
            0x00
        }
    }
    fn write8(&mut self, addr: u16, data: u8) {
        if self.regs_enable && self.in_regs(addr) {
            self.regs_write(addr, data);
            return;
        }
        if let Some(paddr) = self.translate(addr) {
            // Write-protect enforcement
            let page = (addr as usize) >> PAGE_SHIFT;
            let attr = if self.sys_mode {
                self.attr_sys[self.task as usize][page]
            } else {
                self.attr_user[self.task as usize][page]
            };
            let wprot = (self.prot_ctrl & 0x01) != 0 && (attr & 0x01) != 0;
            if wprot {
                self.fault_status |= 0x01;
            } else {
                self.phys[paddr] = data;
            }
        }
    }
    fn irq_lines(&mut self) -> (bool, bool, bool) {
        // Fault -> IRQ/FIRQ if enabled
        let any_fault = (self.fault_status & 0x07) != 0;
        if !any_fault {
            return (false, false, false);
        }
        let use_firq = (self.prot_ctrl & 0x10) != 0; // prefer FIRQ if set
        let irq_on = (self.prot_ctrl & 0x02) != 0;
        let fire = irq_on && any_fault;
        if use_firq {
            (false, fire, false)
        } else {
            (false, false, fire)
        }
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
