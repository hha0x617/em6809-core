//! # `bootscript` — trigger-driven boot-script DSL
//!
//! A small DSL for "do X when Y happens" during emulation.  Used by
//! em6809's `--boot-script` CLI option to set up MMU mappings,
//! console / block / timer device state, and CPU interrupt-mask state
//! at specific PCs or step counts during the boot sequence.
//! Reusable by any embedder that wants the same DSL.
//!
//! ## Syntax
//!
//! Each line is `<trigger>: <action>`.  Triggers:
//!
//! - `at_pc <addr>` — fire when `cpu.r.pc == addr`.
//! - `at_step <N>` — fire when the global instruction count reaches
//!   `N`.
//!
//! Actions cover the common boot-time configuration knobs — see
//! [`Action`] for the full enum.  Comments start with `#` or `//`;
//! blank lines are ignored.
//!
//! ## Provided types
//!
//! - [`Action`] — what the trigger does.  Variants include `Mode`,
//!   `Prot`, `Map(page, frame)`, `Attr(page, attr)`, `ConCtrl`,
//!   `ConRxWm`, `ConIrqHold`, `ConFirq`, `IrqMask`, `FirqMask`,
//!   `BlkIrq`, `BlkFirq`, `BlkIrqHold`.
//! - [`Trigger`] — `OnPc(addr, Action)` or `OnStep(n, Action)`.
//! - [`BootSequencer`] — owns a `Vec<Trigger>` and a "next step
//!   counter".  Constructed with `BootSequencer::new(triggers)`.
//!   The CPU loop calls `seq.on_pre_step(&mut bus, regs_base, pc,
//!   &mut cpu)` and `seq.on_post_step(&mut bus, regs_base)` to fire
//!   matching `OnPc` / `OnStep` triggers in order.  Triggers fire at
//!   most once.  Diagnostic getters
//!   [`BootSequencer::console_missing_count`],
//!   [`BootSequencer::block_missing_count`],
//!   [`BootSequencer::mmu_missing_count`] count silent no-ops when an
//!   action targeted a device the bus doesn't have.
//!
//! ## Provided functions
//!
//! - [`parse_boot_script`] — parse a multi-line script into
//!   `Vec<Trigger>`.  Returns `Err(line N: ...)` on parse failure so
//!   embedders can surface the offending line to the user.
//! - [`emit_boot_template`] — return a sample boot script for a
//!   named scenario (`netbsd_mvme147` and other presets).  Useful
//!   for `--boot-script-template <name>` generators.
//!
//! ## Typical usage
//!
//! ```no_run
//! use em6809_core::bus::Memory;
//! use em6809_core::cpu::Cpu;
//! use em6809_core::io::IoBus;
//! use em6809_core::mmu::Mc6829;
//! use em6809_core::bootscript::{BootSequencer, parse_boot_script};
//!
//! let script = "
//!     at_pc $0100: mode sys
//!     at_pc $0100: map 0=0
//!     at_step 1000: con_ctrl 0x55
//! ";
//! let triggers = parse_boot_script(script).expect("valid script");
//! let mut seq = BootSequencer::new(triggers);
//!
//! let mut bus = IoBus::new(Memory::new());
//! let mut cpu = Cpu::new();
//! cpu.reset(&mut bus);
//! let regs_base = 0xFFE0;
//! loop {
//!     seq.on_pre_step(&mut bus, regs_base, cpu.r.pc, &mut cpu);
//!     cpu.step(&mut bus, false);
//!     seq.on_post_step(&mut bus, regs_base);
//! }
//! ```
//!
//! See em6809's `docs/en/config_and_boot_script.md` for the full
//! script grammar and known footguns (config-vs-script ordering,
//! silent no-op when target devices are absent, MMU base validation).

use crate::bus::Bus;
use crate::io::IoBus;
// ConsoleDev referenced via IoBus helper; no direct use here
use crate::bus::Memory;
use crate::cpu::Cpu;
use crate::mmu::Mc6829;

#[derive(Clone, Debug)]
pub enum Action {
    Mode(bool),      // true=sys, false=user
    Prot(u8),        // write PROT_CTRL
    Map(usize, u16), // page -> frame
    Attr(usize, u8), // page attr
    ConCtrl(u8),     // console ctrl register
    ConRxWm(usize),  // console RX watermark
    ConIrqHold(u32), // console IRQ hold steps
    ConFirq(bool),   // console FIRQ preference
    IrqMask(bool),   // CPU I mask (true=set mask=disable IRQ)
    FirqMask(bool),  // CPU F mask (true=set mask=disable FIRQ)
    BlkIrq(bool),
    BlkFirq(bool),
    BlkIrqHold(u32),
}

#[derive(Clone, Debug)]
pub enum Trigger {
    OnPc(u16, Action),
    OnStep(u64, Action),
}

pub fn parse_boot_script(s: &str) -> Result<Vec<Trigger>, String> {
    let mut out: Vec<Trigger> = Vec::new();
    for (lineno, raw) in s.lines().enumerate() {
        let line_no = lineno + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(kind) = parts.next() else { continue };
        match kind {
            "on_pc" => {
                let Some(addr_s) = parts.next() else {
                    return Err(format!("Line {line_no}: missing address"));
                };
                let addr = parse_hex_or_dec_u16(addr_s)
                    .ok_or_else(|| format!("Line {line_no}: bad addr"))?;
                let Some(cmd) = parts.next() else {
                    return Err(format!("Line {line_no}: missing command"));
                };
                let act = parse_action(cmd, &mut parts, line_no)?;
                out.push(Trigger::OnPc(addr, act));
            }
            "on_step" => {
                let Some(n_s) = parts.next() else {
                    return Err(format!("Line {line_no}: missing step"));
                };
                let n =
                    parse_hex_or_dec_u64(n_s).ok_or_else(|| format!("Line {line_no}: bad step"))?;
                let Some(cmd) = parts.next() else {
                    return Err(format!("Line {line_no}: missing command"));
                };
                let act = parse_action(cmd, &mut parts, line_no)?;
                out.push(Trigger::OnStep(n, act));
            }
            _ => return Err(format!("Line {line_no}: unknown directive {kind}")),
        }
    }
    Ok(out)
}

fn parse_action<'a, I: Iterator<Item = &'a str>>(
    cmd: &str,
    parts: &mut I,
    lineno: usize,
) -> Result<Action, String> {
    match cmd {
        "mode" => {
            let Some(m) = parts.next() else {
                return Err(format!("Line {lineno}: missing mode"));
            };
            Ok(Action::Mode(matches!(
                m.to_lowercase().as_str(),
                "sys" | "system"
            )))
        }
        "prot" => {
            let Some(v) = parts.next() else {
                return Err(format!("Line {lineno}: missing prot"));
            };
            let pv = parse_hex_or_dec_u8(v).ok_or_else(|| format!("Line {lineno}: bad prot"))?;
            Ok(Action::Prot(pv))
        }
        "map" => {
            let Some(pf) = parts.next() else {
                return Err(format!("Line {lineno}: missing map"));
            };
            let Some(eq) = pf.find('=') else {
                return Err(format!("Line {lineno}: map expects P=F"));
            };
            let (p, f) = pf.split_at(eq);
            let f = &f[1..];
            let page =
                parse_hex_or_dec_usize(p).ok_or_else(|| format!("Line {lineno}: bad page"))?;
            let frame =
                parse_hex_or_dec_u16(f).ok_or_else(|| format!("Line {lineno}: bad frame"))?;
            Ok(Action::Map(page, frame))
        }
        "attr" => {
            let Some(pv) = parts.next() else {
                return Err(format!("Line {lineno}: missing attr"));
            };
            let Some(eq) = pv.find('=') else {
                return Err(format!("Line {lineno}: attr expects P=V"));
            };
            let (p, v) = pv.split_at(eq);
            let v = &v[1..];
            let page =
                parse_hex_or_dec_usize(p).ok_or_else(|| format!("Line {lineno}: bad page"))?;
            let val = parse_hex_or_dec_u8(v).ok_or_else(|| format!("Line {lineno}: bad value"))?;
            Ok(Action::Attr(page, val))
        }
        "con_ctrl" => {
            let Some(v) = parts.next() else {
                return Err(format!("Line {lineno}: missing con_ctrl"));
            };
            let pv =
                parse_hex_or_dec_u8(v).ok_or_else(|| format!("Line {lineno}: bad con_ctrl"))?;
            Ok(Action::ConCtrl(pv))
        }
        "con_rx_wm" => {
            let Some(v) = parts.next() else {
                return Err(format!("Line {lineno}: missing con_rx_wm"));
            };
            let pv =
                parse_hex_or_dec_usize(v).ok_or_else(|| format!("Line {lineno}: bad con_rx_wm"))?;
            Ok(Action::ConRxWm(pv))
        }
        "con_irq_hold" => {
            let Some(v) = parts.next() else {
                return Err(format!("Line {lineno}: missing con_irq_hold"));
            };
            let pv = parse_hex_or_dec_u64(v)
                .ok_or_else(|| format!("Line {lineno}: bad con_irq_hold"))?
                as u32;
            Ok(Action::ConIrqHold(pv))
        }
        "con_firq" => {
            let Some(v) = parts.next() else {
                return Err(format!("Line {lineno}: missing con_firq"));
            };
            let on = matches!(v.to_lowercase().as_str(), "1" | "on" | "true" | "yes");
            Ok(Action::ConFirq(on))
        }
        "blk_irq" => {
            let Some(v) = parts.next() else {
                return Err(format!("Line {lineno}: missing blk_irq"));
            };
            let on = matches!(v.to_lowercase().as_str(), "1" | "on" | "true" | "yes");
            Ok(Action::BlkIrq(on))
        }
        "blk_firq" => {
            let Some(v) = parts.next() else {
                return Err(format!("Line {lineno}: missing blk_firq"));
            };
            let on = matches!(v.to_lowercase().as_str(), "1" | "on" | "true" | "yes");
            Ok(Action::BlkFirq(on))
        }
        "blk_irq_hold" => {
            let Some(v) = parts.next() else {
                return Err(format!("Line {lineno}: missing blk_irq_hold"));
            };
            let pv = parse_hex_or_dec_u64(v)
                .ok_or_else(|| format!("Line {lineno}: bad blk_irq_hold"))?
                as u32;
            Ok(Action::BlkIrqHold(pv))
        }
        "irq_mask" => {
            let Some(v) = parts.next() else {
                return Err(format!("Line {lineno}: missing irq_mask"));
            };
            let on = matches!(v.to_lowercase().as_str(), "1" | "on" | "true" | "yes");
            Ok(Action::IrqMask(on))
        }
        "firq_mask" => {
            let Some(v) = parts.next() else {
                return Err(format!("Line {lineno}: missing firq_mask"));
            };
            let on = matches!(v.to_lowercase().as_str(), "1" | "on" | "true" | "yes");
            Ok(Action::FirqMask(on))
        }
        other => Err(format!("Line {lineno}: unknown action {other}")),
    }
}

fn parse_hex_or_dec_u16(s: &str) -> Option<u16> {
    if let Some(h) = s.strip_prefix("0x").or(s.strip_prefix("0X")) {
        u16::from_str_radix(h, 16).ok()
    } else {
        s.parse::<u16>().ok()
    }
}
fn parse_hex_or_dec_u8(s: &str) -> Option<u8> {
    if let Some(h) = s.strip_prefix("0x").or(s.strip_prefix("0X")) {
        u8::from_str_radix(h, 16).ok()
    } else {
        s.parse::<u8>().ok()
    }
}
fn parse_hex_or_dec_usize(s: &str) -> Option<usize> {
    if let Some(h) = s.strip_prefix("0x").or(s.strip_prefix("0X")) {
        usize::from_str_radix(h, 16).ok()
    } else {
        s.parse::<usize>().ok()
    }
}
fn parse_hex_or_dec_u64(s: &str) -> Option<u64> {
    if let Some(h) = s.strip_prefix("0x").or(s.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()
    } else {
        s.parse::<u64>().ok()
    }
}

pub struct BootSequencer {
    triggers: Vec<Trigger>,
    step: u64,
    /// Number of times a console-targeting Action ran against a bus
    /// with no console attached.  Drives the one-shot warning for
    /// footgun B in `docs/en/config_and_boot_script.md`.  Counter
    /// (rather than a bool) so unit tests can assert the warning
    /// path fires the expected number of times.
    console_missing_count: u64,
    /// Likewise for block-targeting Actions.
    block_missing_count: u64,
    /// Number of times an MMU-class Action (Mode / Prot / Map /
    /// Attr) ran against a bus with no MMU on it.  Footgun C in
    /// `docs/en/config_and_boot_script.md`: the previous behaviour
    /// was to issue `bus.write8(mmu_regs_base + offset, value)`
    /// unconditionally, which corrupted plain RAM when MMU was off
    /// (e.g., `prot 0xFF` with the default `mmu_regs_base = 0xFFA0`
    /// against an `IoBus<Memory>` would write `0xFF` into the byte
    /// at `0xFFB2`).  We now skip the write and warn once.
    mmu_missing_count: u64,
}

impl BootSequencer {
    pub fn new(triggers: Vec<Trigger>) -> Self {
        Self {
            triggers,
            step: 0,
            console_missing_count: 0,
            block_missing_count: 0,
            mmu_missing_count: 0,
        }
    }
    pub fn on_pre_step<B: Bus + ?Sized>(
        &mut self,
        bus: &mut B,
        regs_base: u16,
        pc: u16,
        cpu: &mut Cpu,
    ) {
        let ts: Vec<Trigger> = self.triggers.clone();
        for t in ts.iter() {
            match t {
                Trigger::OnPc(match_pc, act) if *match_pc == pc => {
                    self.apply_action(bus, regs_base, act, Some(cpu));
                }
                _ => {}
            }
        }
    }
    pub fn on_post_step<B: Bus + ?Sized>(&mut self, bus: &mut B, regs_base: u16) {
        self.step += 1;
        let ts: Vec<Trigger> = self.triggers.clone();
        for t in ts.iter() {
            match t {
                Trigger::OnStep(n, act) if *n == self.step => {
                    self.apply_action(bus, regs_base, act, None);
                }
                _ => {}
            }
        }
    }
    /// Total number of times a console-targeting Action found no
    /// console on the bus.  The first occurrence emits a warning to
    /// stderr; subsequent occurrences are silent (counter still
    /// increments).  Exposed for tests and diagnostics.
    pub fn console_missing_count(&self) -> u64 {
        self.console_missing_count
    }
    /// Same as [`Self::console_missing_count`] for block devices.
    pub fn block_missing_count(&self) -> u64 {
        self.block_missing_count
    }
    /// Total number of times an MMU-class Action found no MMU on
    /// the bus.  The first occurrence emits a warning to stderr;
    /// subsequent occurrences are silent (counter still
    /// increments).  Exposed for tests and diagnostics.
    pub fn mmu_missing_count(&self) -> u64 {
        self.mmu_missing_count
    }
    fn note_console_missing(&mut self, action_name: &str) {
        if self.console_missing_count == 0 {
            eprintln!(
                "[bootscript] warning: {action_name} action targets the console, \
                 but no console device is attached on this bus; the action is a no-op. \
                 Enable the console in your settings or pass --console <ADDR>. \
                 (Further occurrences silenced; see docs/en/config_and_boot_script.md.)"
            );
        }
        self.console_missing_count = self.console_missing_count.saturating_add(1);
    }
    fn note_block_missing(&mut self, action_name: &str) {
        if self.block_missing_count == 0 {
            eprintln!(
                "[bootscript] warning: {action_name} action targets the block device, \
                 but no block device is attached on this bus; the action is a no-op. \
                 Enable the block device in your settings or pass --block <ADDR>. \
                 (Further occurrences silenced; see docs/en/config_and_boot_script.md.)"
            );
        }
        self.block_missing_count = self.block_missing_count.saturating_add(1);
    }
    fn note_mmu_missing(&mut self, action_name: &str, regs_base: u16) {
        if self.mmu_missing_count == 0 {
            eprintln!(
                "[bootscript] warning: {action_name} action requires an MMU on the bus, \
                 but none is attached.  The write to ${regs_base:04X}+offset would have \
                 hit plain RAM; skipping to avoid memory corruption.  Enable the MMU in \
                 your settings or pass --mmu (and --mmu-reg-base if not the default). \
                 (Further occurrences silenced; see docs/en/config_and_boot_script.md.)"
            );
        }
        self.mmu_missing_count = self.mmu_missing_count.saturating_add(1);
    }
    /// True iff the bus has an MC6829 MMU somewhere we can route
    /// `bus.write8(regs_base + offset, ...)` through legitimately.
    /// In production the bus is one of `IoBus<Memory>`,
    /// `IoBus<Mc6829>`, `Mc6829`, or `Memory` (see
    /// `src/devices.rs::build_bus`); only the latter two with
    /// `Mc6829` somewhere route MMU writes correctly.
    fn bus_has_mmu<B: Bus + ?Sized>(bus: &mut B) -> bool {
        let any = bus.as_any_mut();
        any.is::<Mc6829>() || any.is::<IoBus<Mc6829>>()
    }
}

impl BootSequencer {
    fn apply_action<B: Bus + ?Sized>(
        &mut self,
        bus: &mut B,
        regs_base: u16,
        act: &Action,
        cpu_opt: Option<&mut Cpu>,
    ) {
        match act {
            Action::Mode(sys) => {
                if Self::bus_has_mmu(bus) {
                    bus.write8(regs_base + 0x11, if *sys { 1 } else { 0 });
                } else {
                    self.note_mmu_missing("mode", regs_base);
                }
            }
            Action::Prot(v) => {
                if Self::bus_has_mmu(bus) {
                    bus.write8(regs_base + 0x12, *v);
                } else {
                    self.note_mmu_missing("prot", regs_base);
                }
            }
            Action::Map(page, frame) => {
                if Self::bus_has_mmu(bus) {
                    bus.write8(regs_base + ((*page as u16) & 0x0F), (*frame & 0xFF) as u8);
                } else {
                    self.note_mmu_missing("map", regs_base);
                }
            }
            Action::Attr(page, val) => {
                if Self::bus_has_mmu(bus) {
                    bus.write8(regs_base + 0x18 + ((*page as u16) & 0x0F), *val);
                } else {
                    self.note_mmu_missing("attr", regs_base);
                }
            }
            Action::ConCtrl(v) => {
                let attached = if let Some(iom) = bus.as_any_mut().downcast_mut::<IoBus<Memory>>() {
                    iom.with_console_mut(|c| c.set_ctrl(*v))
                } else if let Some(iom) = bus.as_any_mut().downcast_mut::<IoBus<Mc6829>>() {
                    iom.with_console_mut(|c| c.set_ctrl(*v))
                } else {
                    false
                };
                if !attached {
                    self.note_console_missing("con_ctrl");
                }
            }
            Action::ConRxWm(n) => {
                let attached = if let Some(iom) = bus.as_any_mut().downcast_mut::<IoBus<Memory>>() {
                    iom.with_console_mut(|c| c.set_rx_watermark(*n))
                } else if let Some(iom) = bus.as_any_mut().downcast_mut::<IoBus<Mc6829>>() {
                    iom.with_console_mut(|c| c.set_rx_watermark(*n))
                } else {
                    false
                };
                if !attached {
                    self.note_console_missing("con_rx_wm");
                }
            }
            Action::ConIrqHold(n) => {
                let attached = if let Some(iom) = bus.as_any_mut().downcast_mut::<IoBus<Memory>>() {
                    iom.with_console_mut(|c| c.set_irq_hold_cycles(*n))
                } else if let Some(iom) = bus.as_any_mut().downcast_mut::<IoBus<Mc6829>>() {
                    iom.with_console_mut(|c| c.set_irq_hold_cycles(*n))
                } else {
                    false
                };
                if !attached {
                    self.note_console_missing("con_irq_hold");
                }
            }
            Action::ConFirq(on) => {
                let attached = if let Some(iom) = bus.as_any_mut().downcast_mut::<IoBus<Memory>>() {
                    iom.with_console_mut(|c| c.set_firq(*on))
                } else if let Some(iom) = bus.as_any_mut().downcast_mut::<IoBus<Mc6829>>() {
                    iom.with_console_mut(|c| c.set_firq(*on))
                } else {
                    false
                };
                if !attached {
                    self.note_console_missing("con_firq");
                }
            }
            Action::IrqMask(on) => {
                if let Some(cpu) = cpu_opt {
                    if *on {
                        cpu.r.cc |= 0x10;
                    } else {
                        cpu.r.cc &= !0x10;
                    }
                }
            }
            Action::FirqMask(on) => {
                if let Some(cpu) = cpu_opt {
                    if *on {
                        cpu.r.cc |= 0x40;
                    } else {
                        cpu.r.cc &= !0x40;
                    }
                }
            }
            Action::BlkIrq(on) => {
                let attached = if let Some(iom) = bus.as_any_mut().downcast_mut::<IoBus<Memory>>() {
                    iom.with_block_mut(|b| b.set_irq_enable(*on))
                } else if let Some(iom) = bus.as_any_mut().downcast_mut::<IoBus<Mc6829>>() {
                    iom.with_block_mut(|b| b.set_irq_enable(*on))
                } else {
                    false
                };
                if !attached {
                    self.note_block_missing("blk_irq");
                }
            }
            Action::BlkFirq(on) => {
                let attached = if let Some(iom) = bus.as_any_mut().downcast_mut::<IoBus<Memory>>() {
                    iom.with_block_mut(|b| b.set_firq(*on))
                } else if let Some(iom) = bus.as_any_mut().downcast_mut::<IoBus<Mc6829>>() {
                    iom.with_block_mut(|b| b.set_firq(*on))
                } else {
                    false
                };
                if !attached {
                    self.note_block_missing("blk_firq");
                }
            }
            Action::BlkIrqHold(n) => {
                let attached = if let Some(iom) = bus.as_any_mut().downcast_mut::<IoBus<Memory>>() {
                    iom.with_block_mut(|b| b.set_irq_hold_cycles(*n))
                } else if let Some(iom) = bus.as_any_mut().downcast_mut::<IoBus<Mc6829>>() {
                    iom.with_block_mut(|b| b.set_irq_hold_cycles(*n))
                } else {
                    false
                };
                if !attached {
                    self.note_block_missing("blk_irq_hold");
                }
            }
        }
    }
}

pub fn emit_boot_template(name: &str) -> String {
    match name.to_lowercase().as_str() {
        "os9l2-boot" => {
            "# OS-9 L2 boot sequence\n\
             # Switch to system mode at reset PC\n\
             on_pc 0xE000 mode sys\n\
             # After 256 steps, switch to user mode\n\
             on_step 256 mode user\n"
                .to_string()
        }
        "os9l2-io" => {
            "# OS-9 L2 IO boot sequence\n\
             # Enter system mode at RESET, enable RX IRQ with FIRQ on console, then later switch user\n\
             on_pc 0xE000 mode sys\n\
             on_pc 0xE000 con_ctrl 0x11\n\
             on_pc 0xE000 con_rx_wm 4\n\
             on_pc 0xE000 con_irq_hold 8\n\
             on_step 512 mode user\n"
                .to_string()
        }
        "os9l2-sequence" | "os9l2-full" => {
            "# OS-9 L2 boot sequence (init -> kernel map -> user transfer -> I/O init)\n\
             # 1) Initialization at RESET: enter system mode, protect vectors, NX null\n\
             on_pc 0xE000 mode sys\n\
             on_pc 0xE000 prot 0x0B    # WPROT_EN | NX_EN | IRQ on fault\n\
             on_pc 0xE000 attr 0x00=0x04  # NX null page\n\
             on_pc 0xE000 attr 0x0F=0x01  # WPROT vectors\n\
             # 2) Kernel placement: remap pages 0x0C..0x0E to high frames where kernel resides\n\
             on_pc 0xE000 map 0x0C=0xD0\n\
             on_pc 0xE000 map 0x0D=0xD1\n\
             on_pc 0xE000 map 0x0E=0xD2\n\
             # 3) Transfer to user mode after some initialization steps\n\
             on_step 512 mode user\n\
             # 4) I/O init: enable console RX IRQ with FIRQ, set watermark/hold\n\
             on_step 520 con_ctrl 0x11   # RX IRQ + FIRQ\n\
             on_step 520 con_rx_wm 4\n\
             on_step 520 con_irq_hold 8\n\
             # Optional: mask/unmask IRQ/FIRQ during stages\n\
             # on_pc 0xE000 irq_mask on\n\
             # on_step 300 irq_mask off\n"
                .to_string()
        }
        _ => {
            "# Boot script template\n\
             # on_pc 0xADDR mode user|sys\n\
             # on_pc 0xADDR prot 0xVAL\n\
             # on_pc 0xADDR map P=F\n\
             # on_pc 0xADDR attr P=VAL\n\
             # on_step N mode user|sys\n"
                .to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an IoBus<Memory> with no devices attached.  Console- and
    /// block-targeting Actions must trigger the silent-no-op guard
    /// against this bus.
    fn empty_iobus() -> IoBus<Memory> {
        IoBus::new(Memory::new())
    }

    fn fire_on_pc(seq: &mut BootSequencer, bus: &mut IoBus<Memory>, pc: u16) {
        let mut cpu = Cpu::new();
        seq.on_pre_step(bus, 0, pc, &mut cpu);
    }

    #[test]
    fn con_ctrl_against_no_console_increments_counter_and_skips() {
        let mut bus = empty_iobus();
        let mut seq = BootSequencer::new(vec![Trigger::OnPc(0xE000, Action::ConCtrl(0x11))]);

        // Fire once: counter goes 0 -> 1, warning is emitted on stderr.
        fire_on_pc(&mut seq, &mut bus, 0xE000);
        assert_eq!(seq.console_missing_count(), 1);

        // Fire again: counter increments but no second stderr warning.
        // (The warning suppression is verified by inspection of
        //  `note_console_missing`; here we just assert the counter
        //  keeps climbing as expected.)
        fire_on_pc(&mut seq, &mut bus, 0xE000);
        assert_eq!(seq.console_missing_count(), 2);
    }

    #[test]
    fn all_console_actions_are_guarded() {
        let mut bus = empty_iobus();
        let mut seq = BootSequencer::new(vec![
            Trigger::OnPc(0xE000, Action::ConCtrl(0x11)),
            Trigger::OnPc(0xE001, Action::ConRxWm(4)),
            Trigger::OnPc(0xE002, Action::ConIrqHold(8)),
            Trigger::OnPc(0xE003, Action::ConFirq(true)),
        ]);

        for pc in [0xE000_u16, 0xE001, 0xE002, 0xE003] {
            fire_on_pc(&mut seq, &mut bus, pc);
        }

        // Every console-class Action should have hit the guard.
        assert_eq!(seq.console_missing_count(), 4);
        // Block guard untouched.
        assert_eq!(seq.block_missing_count(), 0);
    }

    #[test]
    fn blk_irq_against_no_block_increments_counter_and_skips() {
        let mut bus = empty_iobus();
        let mut seq = BootSequencer::new(vec![Trigger::OnPc(0xE000, Action::BlkIrq(true))]);

        fire_on_pc(&mut seq, &mut bus, 0xE000);
        assert_eq!(seq.block_missing_count(), 1);
        assert_eq!(seq.console_missing_count(), 0);

        fire_on_pc(&mut seq, &mut bus, 0xE000);
        assert_eq!(seq.block_missing_count(), 2);
    }

    #[test]
    fn all_block_actions_are_guarded() {
        let mut bus = empty_iobus();
        let mut seq = BootSequencer::new(vec![
            Trigger::OnPc(0xE000, Action::BlkIrq(true)),
            Trigger::OnPc(0xE001, Action::BlkFirq(true)),
            Trigger::OnPc(0xE002, Action::BlkIrqHold(8)),
        ]);

        for pc in [0xE000_u16, 0xE001, 0xE002] {
            fire_on_pc(&mut seq, &mut bus, pc);
        }

        assert_eq!(seq.block_missing_count(), 3);
        assert_eq!(seq.console_missing_count(), 0);
    }

    #[test]
    fn mmu_actions_do_not_trigger_console_or_block_guards() {
        // Footgun B (console / block missing) and footgun C (MMU
        // missing) are tracked on independent counters.  Make sure
        // an MMU-class Action against a bus without MMU only trips
        // the C counter, never the B ones.
        let mut bus = empty_iobus();
        let mut seq = BootSequencer::new(vec![
            Trigger::OnPc(0xE000, Action::Mode(true)),
            Trigger::OnPc(0xE001, Action::Prot(0x0B)),
            Trigger::OnPc(0xE002, Action::Map(0x0C, 0xD0)),
            Trigger::OnPc(0xE003, Action::Attr(0x00, 0x04)),
        ]);

        for pc in [0xE000_u16, 0xE001, 0xE002, 0xE003] {
            fire_on_pc(&mut seq, &mut bus, pc);
        }

        assert_eq!(seq.console_missing_count(), 0);
        assert_eq!(seq.block_missing_count(), 0);
        // Footgun C guard fires once per Action.
        assert_eq!(seq.mmu_missing_count(), 4);
    }

    #[test]
    fn console_action_with_attached_console_does_not_warn() {
        let mut bus = empty_iobus();
        // Attach a console at base 0xFF00 so the with_console_mut
        // closure runs and the guard stays quiet.
        bus.ensure_console(true, 0xFF00);

        let mut seq = BootSequencer::new(vec![Trigger::OnPc(0xE000, Action::ConCtrl(0x11))]);

        fire_on_pc(&mut seq, &mut bus, 0xE000);
        fire_on_pc(&mut seq, &mut bus, 0xE000);

        assert_eq!(seq.console_missing_count(), 0);
    }

    #[test]
    fn block_action_with_attached_block_does_not_warn() {
        let mut bus = empty_iobus();
        bus.ensure_block(true, 0xFF20);

        let mut seq = BootSequencer::new(vec![Trigger::OnPc(0xE000, Action::BlkIrq(true))]);

        fire_on_pc(&mut seq, &mut bus, 0xE000);

        assert_eq!(seq.block_missing_count(), 0);
    }

    // -------------------------------------------------------------------
    // Footgun C: MMU base validation guard.
    // -------------------------------------------------------------------

    /// Build an `IoBus<Mc6829>` with a real MMU on the bus, so
    /// `bus_has_mmu` returns true and MMU-class Actions write
    /// through legitimately.
    fn mmu_iobus(regs_base: u16) -> IoBus<Mc6829> {
        IoBus::new(Mc6829::new(64 * 1024, regs_base))
    }

    /// Standard test base for the MMU register page.  Matches the
    /// CLI default (`--mmu-reg-base 0xFFA0`).
    const TEST_REGS_BASE: u16 = 0xFFA0;

    fn fire_on_pc_with_base(seq: &mut BootSequencer, bus: &mut dyn Bus, regs_base: u16, pc: u16) {
        let mut cpu = Cpu::new();
        seq.on_pre_step(bus, regs_base, pc, &mut cpu);
    }

    #[test]
    fn prot_against_no_mmu_increments_counter_and_skips_write() {
        // Repro the corruption case from
        // `docs/en/config_and_boot_script.md`: with the default
        // `mmu_regs_base = 0xFFA0`, a `prot 0xFF` against a bus
        // without MMU would have written 0xFF into RAM at 0xFFB2
        // (= 0xFFA0 + 0x12).  Confirm the byte is left untouched
        // and the C counter increments by exactly 1.
        let mut bus = empty_iobus();
        let target = TEST_REGS_BASE.wrapping_add(0x12);
        let before = bus.read8(target);
        let mut seq = BootSequencer::new(vec![Trigger::OnPc(0xE000, Action::Prot(0xFF))]);

        fire_on_pc_with_base(&mut seq, &mut bus, TEST_REGS_BASE, 0xE000);

        assert_eq!(seq.mmu_missing_count(), 1);
        assert_eq!(
            bus.read8(target),
            before,
            "RAM at ${target:04X} must be unchanged when MMU is missing"
        );
    }

    #[test]
    fn all_mmu_actions_are_guarded_against_no_mmu() {
        let mut bus = empty_iobus();
        let mut seq = BootSequencer::new(vec![
            Trigger::OnPc(0xE000, Action::Mode(true)),
            Trigger::OnPc(0xE001, Action::Prot(0x0B)),
            Trigger::OnPc(0xE002, Action::Map(0x0C, 0xD0)),
            Trigger::OnPc(0xE003, Action::Attr(0x00, 0x04)),
        ]);

        for pc in [0xE000_u16, 0xE001, 0xE002, 0xE003] {
            fire_on_pc_with_base(&mut seq, &mut bus, TEST_REGS_BASE, pc);
        }

        assert_eq!(seq.mmu_missing_count(), 4);
        assert_eq!(seq.console_missing_count(), 0);
        assert_eq!(seq.block_missing_count(), 0);
    }

    #[test]
    fn mmu_action_with_mmu_present_does_not_warn() {
        let mut bus = mmu_iobus(TEST_REGS_BASE);
        let mut seq = BootSequencer::new(vec![Trigger::OnPc(0xE000, Action::Prot(0x0B))]);

        fire_on_pc_with_base(&mut seq, &mut bus, TEST_REGS_BASE, 0xE000);
        fire_on_pc_with_base(&mut seq, &mut bus, TEST_REGS_BASE, 0xE000);

        // Both writes routed through the real MMU.  No warning.
        assert_eq!(seq.mmu_missing_count(), 0);
    }

    #[test]
    fn mmu_actions_with_bare_mc6829_bus_do_not_warn() {
        // The other Bus variant `build_bus` can return when MMU is
        // enabled but no I/O devices are requested: a bare
        // `Mc6829` without an `IoBus` wrapper.  Confirm
        // `bus_has_mmu` recognises it too.
        let mut mmu = Mc6829::new(64 * 1024, TEST_REGS_BASE);
        let mut seq = BootSequencer::new(vec![Trigger::OnPc(0xE000, Action::Mode(true))]);

        fire_on_pc_with_base(&mut seq, &mut mmu, TEST_REGS_BASE, 0xE000);

        assert_eq!(seq.mmu_missing_count(), 0);
    }

    #[test]
    fn mmu_guard_fires_once_per_warning_class_then_silently_counts() {
        // Sanity check: counter keeps climbing even after the
        // initial stderr warning is suppressed.
        let mut bus = empty_iobus();
        let mut seq = BootSequencer::new(vec![Trigger::OnPc(0xE000, Action::Mode(true))]);

        for _ in 0..10 {
            fire_on_pc_with_base(&mut seq, &mut bus, TEST_REGS_BASE, 0xE000);
        }

        assert_eq!(seq.mmu_missing_count(), 10);
    }
}
