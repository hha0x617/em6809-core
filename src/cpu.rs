#![allow(clippy::uninlined_format_args)]
use crate::bus::Bus;
use crate::debug::{BreakpointSet, CallFrame, CallKind, ShadowCallStack};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Default)]
pub struct Registers {
    pub a: u8,
    pub b: u8,
    pub x: u16,
    pub y: u16,
    pub u: u16,
    pub s: u16,
    pub pc: u16,
    pub dp: u8,
    pub cc: u8, // E F H I N Z V C
}

pub struct Cpu {
    pub r: Registers,
    pub cycles: u64,
    // pending interrupt latches
    pub nmi_pending: bool,
    pub firq_pending: bool,
    pub irq_pending: bool,
    /// Shadow call stack — pushed by BSR/LBSR/JSR/SWI/IRQ/FIRQ/NMI
    /// handlers inside `step`, popped by RTS/RTI. Owned by `Cpu` so
    /// the call/return paths can update it inline without going
    /// through trait objects. UI layers read this via
    /// `cpu.shadow_stack.frames()`.
    pub shadow_stack: ShadowCallStack,
    /// Execution-breakpoint set. The run loop is expected to call
    /// `breakpoints.should_break(cpu.r.pc)` *before* each `step` and
    /// pause execution if it returns `Some(id)`. Kept on `Cpu` (not
    /// in a higher-level runtime) so emfe plugins that embed the
    /// core can expose breakpoints through the same struct without a
    /// parallel storage layer.
    pub breakpoints: BreakpointSet,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

// CC flag bits
const E: u8 = 0x80;
const F: u8 = 0x40;
const H: u8 = 0x20;
const I: u8 = 0x10;
const N: u8 = 0x08;
const Z: u8 = 0x04;
const V: u8 = 0x02;
const C: u8 = 0x01;

// Global IRQ/FIRQ/NMI debug logging toggle
static DEBUG_IRQ_LOG: AtomicBool = AtomicBool::new(false);
pub fn set_irq_log(on: bool) {
    DEBUG_IRQ_LOG.store(on, Ordering::SeqCst);
}

/// Outcome of `Cpu::step_over` / `Cpu::step_out`. The variant tells
/// callers (UI / scripted tests) why the step terminated, so they can
/// distinguish "ran to the expected target" from "a breakpoint got in
/// the way" or "we gave up after `limit` instructions".
///
/// The CPU's PC is left at the address where execution stopped in
/// every variant.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStop {
    /// `step_over`: PC reached the instruction immediately after the
    /// CALL we stepped over. `step_out`: PC reached the topmost
    /// shadow-frame's `return_addr`.
    ReturnTarget,
    /// A breakpoint fired before we reached the return target. The
    /// `BreakpointId` is whichever BP `BreakpointSet::check` selected.
    Breakpoint(crate::debug::BreakpointId),
    /// `limit` instructions executed without reaching the return
    /// target. Caller should treat this like a hit-the-wall stop —
    /// likely a runaway callee that never returns.
    Limit,
    /// `step_over` only: the instruction at PC was not a CALL, so we
    /// just executed it once and stopped — same observable result as
    /// a plain step.
    NotACall,
    /// `step_out` only: the shadow stack was empty, so there's no
    /// caller frame to step out to. Caller (UI) should treat this as
    /// "no-op, button should have been disabled".
    EmptyStack,
}

/// MC6809 "call" opcodes that `step_over` should descend into:
///   $8D       BSR  rel8
///   $17       LBSR rel16
///   $9D       JSR  direct
///   $AD       JSR  indexed   (variable length via postbyte)
///   $BD       JSR  extended
///   $3F       SWI
///   $10 $3F   SWI2
///   $11 $3F   SWI3
fn is_call_instruction<B: Bus + ?Sized>(bus: &mut B, pc: u16) -> bool {
    let op = bus.read8(pc);
    match op {
        0x8D | 0x17 | 0x9D | 0xAD | 0xBD | 0x3F => true,
        0x10 | 0x11 => bus.read8(pc.wrapping_add(1)) == 0x3F,
        _ => false,
    }
}

impl Cpu {
    pub fn new() -> Self {
        Self {
            r: Registers::default(),
            cycles: 0,
            nmi_pending: false,
            firq_pending: false,
            irq_pending: false,
            shadow_stack: ShadowCallStack::new(),
            breakpoints: BreakpointSet::new(),
        }
    }

    /// Run-loop entry point for execution-breakpoint checking. Wraps
    /// `BreakpointSet::check`, threading `&self.r` through so the
    /// condition evaluator sees the live register state.
    pub fn check_breakpoint(&mut self, pc: u16) -> Option<crate::debug::BreakpointId> {
        // Field-level disjoint borrow: `self.breakpoints` is mutably
        // borrowed and `self.r` is immutably borrowed in the same
        // expression. Rust 2021+ accepts this because the two fields
        // are non-overlapping.
        self.breakpoints.check(pc, &self.r)
    }

    pub fn reset<B: Bus + ?Sized>(&mut self, bus: &mut B) {
        // PC from reset vector at FFFE/FFFF
        let pch = bus.read8(0xFFFE);
        let pcl = bus.read8(0xFFFF);
        self.r.pc = ((pch as u16) << 8) | (pcl as u16);
        self.r.cc = I | F; // mask interrupts by default
        self.r.dp = 0;
        // Frames recorded before reset belong to a program that has
        // just gone away — keeping them would make the call-stack
        // pane lie about who's currently executing.
        self.shadow_stack.clear();
    }

    pub fn set_pc(&mut self, pc: u16) {
        self.r.pc = pc;
    }

    /// Step over the instruction at PC. If it's a CALL (BSR / LBSR /
    /// JSR / SWI / SWI2 / SWI3), execute until PC reaches the byte
    /// right after the CALL instruction, a breakpoint fires, or
    /// `limit` instructions have been retired without reaching the
    /// target. Otherwise behaves like a single `step` and returns
    /// `StepStop::NotACall`.
    ///
    /// The `limit` is a guard against runaway callees (e.g., a step-
    /// over of a `bsr loop_forever`). Callers typically pass something
    /// like `2_000_000`; tests pick small values to exercise the
    /// `Limit` branch directly.
    pub fn step_over<B: Bus + ?Sized>(&mut self, bus: &mut B, limit: u32) -> StepStop {
        let pc0 = self.r.pc;
        if !is_call_instruction(bus, pc0) {
            let _ = self.step(bus, false);
            return StepStop::NotACall;
        }

        // Compute the return target via the disassembler so indexed
        // JSR (and the multi-byte SWI2/SWI3 prefixes) are handled
        // uniformly. `len` is the byte length of the instruction at
        // pc0; the call's RTS/RTI will eventually transfer control to
        // pc0 + len.
        let (len, _) = crate::disasm::disasm_one(bus, pc0);
        let return_target = pc0.wrapping_add(len);

        // Always execute the CALL itself first so we descend into the
        // callee — otherwise `step_over` on a CALL with no further
        // breakpoints would just sit at pc0 forever.
        let _ = self.step(bus, false);

        let mut count = 1u32;
        while count < limit {
            if self.r.pc == return_target {
                return StepStop::ReturnTarget;
            }
            if let Some(id) = self.check_breakpoint(self.r.pc) {
                return StepStop::Breakpoint(id);
            }
            let _ = self.step(bus, false);
            count += 1;
        }
        StepStop::Limit
    }

    /// Run until the current callee returns to its caller. Uses the
    /// topmost shadow-stack frame's `return_addr` as the target — this
    /// is robust across both RTS-style returns (where S happens to
    /// hold the return PC) and SWI/IRQ-style returns (where the S
    /// stack top is the saved CC byte, not a PC). Stops early on
    /// breakpoint hit or `limit` exhaustion.
    pub fn step_out<B: Bus + ?Sized>(&mut self, bus: &mut B, limit: u32) -> StepStop {
        let return_target = match self.shadow_stack.peek() {
            Some(frame) => frame.return_addr,
            None => return StepStop::EmptyStack,
        };

        let mut count = 0u32;
        while count < limit {
            if self.r.pc == return_target {
                return StepStop::ReturnTarget;
            }
            if let Some(id) = self.check_breakpoint(self.r.pc) {
                return StepStop::Breakpoint(id);
            }
            let _ = self.step(bus, false);
            count += 1;
        }
        StepStop::Limit
    }

    pub fn request_nmi(&mut self) {
        self.nmi_pending = true;
    }
    pub fn request_firq(&mut self) {
        self.firq_pending = true;
    }
    pub fn request_irq(&mut self) {
        self.irq_pending = true;
    }

    fn fetch8<B: Bus + ?Sized>(&mut self, bus: &mut B) -> u8 {
        let v = bus.read8_fetch(self.r.pc);
        self.r.pc = self.r.pc.wrapping_add(1);
        v
    }
    fn fetch16<B: Bus + ?Sized>(&mut self, bus: &mut B) -> u16 {
        let hi = self.fetch8(bus) as u16;
        let lo = self.fetch8(bus) as u16;
        (hi << 8) | lo
    }

    fn ea_direct(&self, zp: u8) -> u16 {
        ((self.r.dp as u16) << 8) | (zp as u16)
    }
    fn ea_extended<B: Bus + ?Sized>(&mut self, bus: &mut B) -> u16 {
        self.fetch16(bus)
    }

    fn get_idx_reg(&self, code: u8) -> u16 {
        match code & 0x03 {
            0 => self.r.x,
            1 => self.r.y,
            2 => self.r.u,
            _ => self.r.s,
        }
    }
    fn set_idx_reg(&mut self, code: u8, val: u16) {
        match code & 0x03 {
            0 => self.r.x = val,
            1 => self.r.y = val,
            2 => self.r.u = val,
            _ => self.r.s = val,
        }
    }

    // Decode subset of 6809 indexed addressing postbyte; returns (EA, extra_cycles, desc)
    fn ea_indexed<B: Bus + ?Sized>(&mut self, bus: &mut B) -> (u16, u32, String) {
        let pb = self.fetch8(bus);
        let rsel = (pb >> 5) & 0x03; // 0:X 1:Y 2:U 3:S
        if (pb & 0x80) == 0 {
            // 5-bit signed offset
            let base = self.get_idx_reg(rsel);
            let off5 = ((pb & 0x1F) as i8) << 3 >> 3; // sign-extend 5-bit
            let ea = base.wrapping_add(off5 as u16);
            let reg = match rsel {
                0 => "X",
                1 => "Y",
                2 => "U",
                _ => "S",
            };
            let desc = format!("{}+{}", reg, off5);
            return (ea, 0, desc); // base indexed cost will be applied by caller
        }
        // bit7=1: extended forms (partial). Mode lives in the low 4 bits —
        // bit 4 is the indirect flag and must NOT be mixed into the match
        // value (otherwise `,R` indirect ($14) would not match `,R` direct ($04)).
        let form = pb & 0x0F;
        let indirect = (pb & 0x10) != 0; // indirect (bracketed) forms
                                         // PC-relative in MC6809 is encoded by forms 0x0C (8-bit) and
                                         // 0x0D (16-bit); rsel bits are don't-care for these forms.
                                         // Forms 0x04/0x08/0x09/0x0B always use the register selected
                                         // by rsel (X/Y/U/S) — including S.
        let base = self.get_idx_reg(rsel);
        let is_pc_rel = matches!(form, 0x0C | 0x0D);
        match form {
            0x04 => {
                // ,R
                let mut ea = base;
                if indirect {
                    let hi = bus.read8(ea) as u16;
                    let lo = bus.read8(ea.wrapping_add(1)) as u16;
                    ea = (hi << 8) | lo;
                }
                let reg = if is_pc_rel {
                    "PC"
                } else {
                    match rsel {
                        0 => "X",
                        1 => "Y",
                        2 => "U",
                        _ => "S",
                    }
                };
                let desc = if indirect {
                    format!("[{}]", reg)
                } else {
                    format!(",{}", reg)
                };
                let add = if indirect { 3 } else { 0 };
                (ea, add, desc)
            }
            0x05 => {
                // B,R
                let off = self.r.b as i8 as i16;
                let mut ea = (base as i16).wrapping_add(off) as u16;
                if indirect {
                    let hi = bus.read8(ea) as u16;
                    let lo = bus.read8(ea.wrapping_add(1)) as u16;
                    ea = (hi << 8) | lo;
                }
                let reg = if is_pc_rel {
                    "PC"
                } else {
                    match rsel {
                        0 => "X",
                        1 => "Y",
                        2 => "U",
                        _ => "S",
                    }
                };
                let desc = if indirect {
                    format!("[B,{}]", reg)
                } else {
                    format!("B,{}", reg)
                };
                let add = if indirect { 3 } else { 0 };
                (ea, add, desc)
            }
            0x06 => {
                // A,R
                let off = self.r.a as i8 as i16;
                let mut ea = (base as i16).wrapping_add(off) as u16;
                if indirect {
                    let hi = bus.read8(ea) as u16;
                    let lo = bus.read8(ea.wrapping_add(1)) as u16;
                    ea = (hi << 8) | lo;
                }
                let reg = if is_pc_rel {
                    "PC"
                } else {
                    match rsel {
                        0 => "X",
                        1 => "Y",
                        2 => "U",
                        _ => "S",
                    }
                };
                let desc = if indirect {
                    format!("[A,{}]", reg)
                } else {
                    format!("A,{}", reg)
                };
                let add = if indirect { 3 } else { 0 };
                (ea, add, desc)
            }
            0x08 => {
                // 8-bit offset
                let off8 = self.fetch8(bus) as i8 as i16;
                let mut ea = (base as i16).wrapping_add(off8) as u16;
                if indirect {
                    let hi = bus.read8(ea) as u16;
                    let lo = bus.read8(ea.wrapping_add(1)) as u16;
                    ea = (hi << 8) | lo;
                }
                let reg = if is_pc_rel {
                    "PC"
                } else {
                    match rsel {
                        0 => "X",
                        1 => "Y",
                        2 => "U",
                        _ => "S",
                    }
                };
                let desc = if indirect {
                    format!("[{}+{}]", reg, off8)
                } else {
                    format!("{}+{}", reg, off8)
                };
                let add = 1 + if indirect { 3 } else { 0 };
                (ea, add, desc)
            }
            0x09 => {
                // 16-bit offset
                let off16 = self.fetch16(bus) as i16;
                let mut ea = (base as i32 + off16 as i32) as u16;
                if indirect {
                    let hi = bus.read8(ea) as u16;
                    let lo = bus.read8(ea.wrapping_add(1)) as u16;
                    ea = (hi << 8) | lo;
                }
                let reg = if is_pc_rel {
                    "PC"
                } else {
                    match rsel {
                        0 => "X",
                        1 => "Y",
                        2 => "U",
                        _ => "S",
                    }
                };
                let desc = if indirect {
                    format!("[{}+{}]", reg, off16)
                } else {
                    format!("{}+{}", reg, off16)
                };
                let add = 2 + if indirect { 3 } else { 0 };
                (ea, add, desc)
            }
            0x00 => {
                // ,R+
                let ea = base;
                self.set_idx_reg(rsel, base.wrapping_add(1));
                let desc = format!(
                    ",{}+",
                    match rsel {
                        0 => "X",
                        1 => "Y",
                        2 => "U",
                        _ => "S",
                    }
                );
                (ea, 0, desc)
            }
            0x01 => {
                // ,R++
                let ea = base;
                self.set_idx_reg(rsel, base.wrapping_add(2));
                let desc = format!(
                    ",{}++",
                    match rsel {
                        0 => "X",
                        1 => "Y",
                        2 => "U",
                        _ => "S",
                    }
                );
                (ea, 1, desc)
            }
            0x02 => {
                // ,-R
                let ea = base.wrapping_sub(1);
                self.set_idx_reg(rsel, ea);
                let desc = format!(
                    ",-{}",
                    match rsel {
                        0 => "X",
                        1 => "Y",
                        2 => "U",
                        _ => "S",
                    }
                );
                (ea, 0, desc)
            }
            0x03 => {
                // ,--R
                let ea = base.wrapping_sub(2);
                self.set_idx_reg(rsel, ea);
                let desc = format!(
                    ",--{}",
                    match rsel {
                        0 => "X",
                        1 => "Y",
                        2 => "U",
                        _ => "S",
                    }
                );
                (ea, 1, desc)
            }
            0x0B => {
                // D,R (already handled; keep explicit for clarity)
                let off = (((self.r.a as u16) << 8) | self.r.b as u16) as i16;
                let mut ea = (base as i32 + off as i32) as u16;
                if indirect {
                    let hi = bus.read8(ea) as u16;
                    let lo = bus.read8(ea.wrapping_add(1)) as u16;
                    ea = (hi << 8) | lo;
                }
                let reg = if is_pc_rel {
                    "PC"
                } else {
                    match rsel {
                        0 => "X",
                        1 => "Y",
                        2 => "U",
                        _ => "S",
                    }
                };
                let desc = if indirect {
                    format!("[D,{}]", reg)
                } else {
                    format!("D,{}", reg)
                };
                let add = 1 + if indirect { 3 } else { 0 };
                (ea, add, desc)
            }
            0x0C => {
                // n,PCR (8-bit PC-relative)
                let off8 = self.fetch8(bus) as i8 as i16;
                let mut ea = (self.r.pc as i16).wrapping_add(off8) as u16;
                if indirect {
                    let hi = bus.read8(ea) as u16;
                    let lo = bus.read8(ea.wrapping_add(1)) as u16;
                    ea = (hi << 8) | lo;
                }
                let desc = if indirect {
                    format!("[PC+{}]", off8)
                } else {
                    format!("PC+{}", off8)
                };
                let add = 1 + if indirect { 3 } else { 0 };
                (ea, add, desc)
            }
            0x0D => {
                // n,PCR (16-bit PC-relative)
                let off16 = self.fetch16(bus) as i16;
                let mut ea = (self.r.pc as i32 + off16 as i32) as u16;
                if indirect {
                    let hi = bus.read8(ea) as u16;
                    let lo = bus.read8(ea.wrapping_add(1)) as u16;
                    ea = (hi << 8) | lo;
                }
                let desc = if indirect {
                    format!("[PC+{}]", off16)
                } else {
                    format!("PC+{}", off16)
                };
                let add = 5 + if indirect { 3 } else { 0 };
                (ea, add, desc)
            }
            0x0F | 0x1F => {
                // [n16] extended (absolute) indirect pointer
                let addr = self.fetch16(bus);
                // Per 6809, this is always indirect; treat both variants as indirect
                let hi = bus.read8(addr) as u16;
                let lo = bus.read8(addr.wrapping_add(1)) as u16;
                let ea = (hi << 8) | lo;
                let desc = format!("[{:04X}]", addr);
                (ea, 5, desc) // 16-bit offset (2) + indirect (3)
            }
            _ => {
                // Fallback: treat as ,R
                let ea = base;
                let desc = format!(
                    ",{}?",
                    match rsel {
                        0 => "X",
                        1 => "Y",
                        2 => "U",
                        _ => "S",
                    }
                );
                (ea, 0, desc)
            }
        }
    }

    fn set_nz8(&mut self, v: u8) {
        self.r.cc = (self.r.cc & !(N | Z))
            | (if v == 0 { Z } else { 0 })
            | (if (v & 0x80) != 0 { N } else { 0 });
    }
    fn set_nz16(&mut self, v: u16) {
        self.r.cc = (self.r.cc & !(N | Z))
            | (if v == 0 { Z } else { 0 })
            | (if (v & 0x8000) != 0 { N } else { 0 });
    }

    // Stack helpers (S)
    fn push8<B: Bus + ?Sized>(&mut self, bus: &mut B, v: u8) {
        self.r.s = self.r.s.wrapping_sub(1);
        bus.write8(self.r.s, v);
    }
    fn push16<B: Bus + ?Sized>(&mut self, bus: &mut B, v: u16) {
        self.push8(bus, (v & 0x00FF) as u8);
        self.push8(bus, (v >> 8) as u8);
    }
    fn pull8<B: Bus + ?Sized>(&mut self, bus: &mut B) -> u8 {
        let v = bus.read8(self.r.s);
        self.r.s = self.r.s.wrapping_add(1);
        v
    }
    fn pull16<B: Bus + ?Sized>(&mut self, bus: &mut B) -> u16 {
        let hi = self.pull8(bus) as u16;
        let lo = self.pull8(bus) as u16;
        (hi << 8) | lo
    }

    // U-stack helpers
    fn upush8<B: Bus + ?Sized>(&mut self, bus: &mut B, v: u8) {
        self.r.u = self.r.u.wrapping_sub(1);
        bus.write8(self.r.u, v);
    }
    fn upush16<B: Bus + ?Sized>(&mut self, bus: &mut B, v: u16) {
        self.upush8(bus, (v & 0x00FF) as u8);
        self.upush8(bus, (v >> 8) as u8);
    }
    fn upull8<B: Bus + ?Sized>(&mut self, bus: &mut B) -> u8 {
        let v = bus.read8(self.r.u);
        self.r.u = self.r.u.wrapping_add(1);
        v
    }
    fn upull16<B: Bus + ?Sized>(&mut self, bus: &mut B) -> u16 {
        let hi = self.upull8(bus) as u16;
        let lo = self.upull8(bus) as u16;
        (hi << 8) | lo
    }

    fn cond_eval(&self, opcode: u8) -> Option<bool> {
        let c = (self.r.cc & C) != 0;
        let z = (self.r.cc & Z) != 0;
        let v = (self.r.cc & V) != 0;
        let n = (self.r.cc & N) != 0;
        let nv = n ^ v;
        match opcode {
            0x21 => Some(false),     // BRN (never)
            0x22 => Some(!(c || z)), // BHI
            0x23 => Some(c || z),    // BLS
            0x24 => Some(!c),        // BCC/BHS
            0x25 => Some(c),         // BCS/BLO
            0x26 => Some(!z),        // BNE
            0x27 => Some(z),         // BEQ
            0x28 => Some(!v),        // BVC
            0x29 => Some(v),         // BVS
            0x2A => Some(!n),        // BPL
            0x2B => Some(n),         // BMI
            0x2C => Some(!nv),       // BGE
            0x2D => Some(nv),        // BLT
            0x2E => Some(!z && !nv), // BGT
            0x2F => Some(z || nv),   // BLE
            _ => None,
        }
    }

    fn service_nmi<B: Bus + ?Sized>(&mut self, bus: &mut B) -> u32 {
        // NMI: push entire state with CC at top, then jump to vector FFFC/FFFD
        // Order (deepest -> top): PC, U, Y, X, DP, B, A, CC
        let pc_prev = self.r.pc;
        self.r.cc |= E; // ensure full-frame restore via RTI
        self.push16(bus, self.r.pc);
        self.push16(bus, self.r.u);
        self.push16(bus, self.r.y);
        self.push16(bus, self.r.x);
        self.push8(bus, self.r.dp);
        self.push8(bus, self.r.b);
        self.push8(bus, self.r.a);
        self.push8(bus, self.r.cc);
        self.r.cc |= I | F;
        let hi = bus.read8(0xFFFC) as u16;
        let lo = bus.read8(0xFFFD) as u16;
        let vec = (hi << 8) | lo;
        self.r.pc = vec;
        self.nmi_pending = false;
        // Async event — there's no "call site instruction" so we fold
        // it onto pc_prev (the instruction the NMI interrupted).
        self.shadow_stack.push(CallFrame {
            return_addr: pc_prev,
            call_site: pc_prev,
            target: vec,
            sp_at_call: self.r.s,
            kind: CallKind::Nmi,
        });
        if DEBUG_IRQ_LOG.load(Ordering::SeqCst) {
            println!("[irq] NMI: pc=${:04X} -> vec=${:04X}", pc_prev, vec);
        }
        19
    }
    fn service_firq<B: Bus + ?Sized>(&mut self, bus: &mut B) -> u32 {
        // FIRQ: push PC then CC (so CC is at top for RTI), mask FIRQ/IRQ.
        // Vector at $FFF6/$FFF7.
        let pc_prev = self.r.pc;
        self.push16(bus, self.r.pc);
        self.push8(bus, self.r.cc);
        self.r.cc &= !E; // minimal frame
        self.r.cc |= I | F;
        let hi = bus.read8(0xFFF6) as u16;
        let lo = bus.read8(0xFFF7) as u16;
        let vec = (hi << 8) | lo;
        self.r.pc = vec;
        self.firq_pending = false;
        self.shadow_stack.push(CallFrame {
            return_addr: pc_prev,
            call_site: pc_prev,
            target: vec,
            sp_at_call: self.r.s,
            kind: CallKind::Firq,
        });
        if DEBUG_IRQ_LOG.load(Ordering::SeqCst) {
            println!("[irq] FIRQ: pc=${:04X} -> vec=${:04X}", pc_prev, vec);
        }
        10
    }
    fn service_irq<B: Bus + ?Sized>(&mut self, bus: &mut B) -> u32 {
        // IRQ: push entire state with CC at top, then jump to IRQ vector FFF8/FFF9
        // Order (deepest -> top): PC, U, Y, X, DP, B, A, CC
        let pc_prev = self.r.pc;
        self.r.cc |= E; // ensure full-frame restore via RTI
        self.push16(bus, self.r.pc);
        self.push16(bus, self.r.u);
        self.push16(bus, self.r.y);
        self.push16(bus, self.r.x);
        self.push8(bus, self.r.dp);
        self.push8(bus, self.r.b);
        self.push8(bus, self.r.a);
        self.push8(bus, self.r.cc);
        self.r.cc |= I | F;
        let hi = bus.read8(0xFFF8) as u16;
        let lo = bus.read8(0xFFF9) as u16;
        let vec = (hi << 8) | lo;
        self.r.pc = vec;
        self.irq_pending = false;
        self.shadow_stack.push(CallFrame {
            return_addr: pc_prev,
            call_site: pc_prev,
            target: vec,
            sp_at_call: self.r.s,
            kind: CallKind::Irq,
        });
        if DEBUG_IRQ_LOG.load(Ordering::SeqCst) {
            println!("[irq] IRQ: pc=${:04X} -> vec=${:04X}", pc_prev, vec);
        }
        19
    }

    // TFR/EXG helpers
    fn get_reg16_by_code(&self, code: u8) -> Option<u16> {
        match code & 0x0F {
            0x0 => Some(((self.r.a as u16) << 8) | self.r.b as u16), // D
            0x1 => Some(self.r.x),
            0x2 => Some(self.r.y),
            0x3 => Some(self.r.u),
            0x4 => Some(self.r.s),
            0x5 => Some(self.r.pc),
            _ => None,
        }
    }
    fn set_reg16_by_code(&mut self, code: u8, val: u16) -> bool {
        match code & 0x0F {
            0x0 => {
                self.r.a = (val >> 8) as u8;
                self.r.b = (val & 0xFF) as u8;
                true
            }
            0x1 => {
                self.r.x = val;
                true
            }
            0x2 => {
                self.r.y = val;
                true
            }
            0x3 => {
                self.r.u = val;
                true
            }
            0x4 => {
                self.r.s = val;
                true
            }
            0x5 => {
                self.r.pc = val;
                true
            }
            _ => false,
        }
    }
    fn get_reg8_by_code(&self, code: u8) -> Option<u8> {
        match code & 0x0F {
            0x8 => Some(self.r.a),
            0x9 => Some(self.r.b),
            0xA => Some(self.r.cc),
            0xB => Some(self.r.dp),
            _ => None,
        }
    }
    fn set_reg8_by_code(&mut self, code: u8, val: u8) -> bool {
        match code & 0x0F {
            0x8 => {
                self.r.a = val;
                true
            }
            0x9 => {
                self.r.b = val;
                true
            }
            0xA => {
                self.r.cc = val;
                true
            }
            0xB => {
                self.r.dp = val;
                true
            }
            _ => false,
        }
    }

    // ALU helpers
    fn add8_flags(&mut self, a: u8, b: u8) -> u8 {
        let (sum1, c1) = a.overflowing_add(b);
        let half = ((a & 0x0F) + (b & 0x0F)) > 0x0F;
        let ovf = ((a ^ sum1) & (b ^ sum1) & 0x80) != 0;
        self.set_nz8(sum1);
        self.r.cc = (self.r.cc & !(C | V | H))
            | (if c1 { C } else { 0 })
            | (if ovf { V } else { 0 })
            | (if half { H } else { 0 });
        sum1
    }
    fn adc8_flags(&mut self, a: u8, b: u8) -> u8 {
        let cin = if (self.r.cc & C) != 0 { 1 } else { 0 };
        let (t, c1) = a.overflowing_add(b);
        let (sum, c2) = t.overflowing_add(cin);
        let carry = c1 || c2;
        let half = ((a & 0x0F) + (b & 0x0F) + cin) > 0x0F;
        let ovf = (((a ^ sum) & (b ^ sum)) & 0x80) != 0;
        self.set_nz8(sum);
        self.r.cc = (self.r.cc & !(C | V | H))
            | (if carry { C } else { 0 })
            | (if ovf { V } else { 0 })
            | (if half { H } else { 0 });
        sum
    }
    fn sub8_flags(&mut self, a: u8, b: u8) -> u8 {
        let (diff, borrow) = a.overflowing_sub(b);
        // 6809: C=1 indicates borrow occurred on subtract
        let cflag = borrow;
        let ovf = (((a ^ b) & (a ^ diff)) & 0x80) != 0;
        let half = ((a & 0x0F) as i16 - (b & 0x0F) as i16) < 0;
        self.set_nz8(diff);
        self.r.cc = (self.r.cc & !(C | V | H))
            | (if cflag { C } else { 0 })
            | (if ovf { V } else { 0 })
            | (if half { H } else { 0 });
        diff
    }
    fn sbc8_flags(&mut self, a: u8, b: u8) -> u8 {
        // 6809: C=1 indicates borrow.  SBCA/SBCB computes A - M - C (with
        // C used directly as the borrow-in, not inverted).
        let cin = if (self.r.cc & C) != 0 { 1 } else { 0 };
        let (t, b1) = a.overflowing_sub(b);
        let (diff, b2) = t.overflowing_sub(cin);
        let borrow = b1 || b2;
        let cflag = borrow;
        let ovf = (((a ^ b) & (a ^ diff)) & 0x80) != 0;
        let half = ((a & 0x0F) as i16 - (b & 0x0F) as i16 - cin as i16) < 0;
        self.set_nz8(diff);
        self.r.cc = (self.r.cc & !(C | V | H))
            | (if cflag { C } else { 0 })
            | (if ovf { V } else { 0 })
            | (if half { H } else { 0 });
        diff
    }
    fn logic8_nz_clearv(&mut self, v: u8) {
        self.set_nz8(v);
        self.r.cc &= !V;
    }

    fn add16_flags(&mut self, a: u16, b: u16) -> u16 {
        let (sum, carry) = a.overflowing_add(b);
        let ovf = ((a ^ sum) & (b ^ sum) & 0x8000) != 0;
        self.set_nz16(sum);
        self.r.cc =
            (self.r.cc & !(C | V)) | (if carry { C } else { 0 }) | (if ovf { V } else { 0 });
        sum
    }
    fn sub16_flags(&mut self, a: u16, b: u16) -> u16 {
        let (diff, borrow) = a.overflowing_sub(b);
        // 6809: C=1 indicates borrow occurred on subtract
        let cflag = borrow;
        let ovf = (((a ^ b) & (a ^ diff)) & 0x8000) != 0;
        self.set_nz16(diff);
        self.r.cc =
            (self.r.cc & !(C | V)) | (if cflag { C } else { 0 }) | (if ovf { V } else { 0 });
        diff
    }

    pub fn step<B: Bus + ?Sized>(&mut self, bus: &mut B, trace: bool) -> u32 {
        let pc0 = self.r.pc;
        let op = self.fetch8(bus);
        let consumed: u32;
        match op {
            0x1A => {
                // ORCC #imm
                let imm = self.fetch8(bus);
                self.r.cc |= imm;
                consumed = 2;
                if trace {
                    println!("{:04X}: 1A {:02X}   ORCC #${:02X}", pc0, imm, imm);
                }
            }
            0x1C => {
                // ANDCC #imm
                let imm = self.fetch8(bus);
                self.r.cc &= imm;
                consumed = 2;
                if trace {
                    println!("{:04X}: 1C {:02X}   ANDCC #${:02X}", pc0, imm, imm);
                }
            }

            0x10 => {
                // prefix for long conditional branches / SWI2 / CMPD / Y-reg ops
                let sub = self.fetch8(bus);
                if (0x21..=0x2F).contains(&sub) {
                    // Long conditional branches: $10 $21..$2F  (LBRN..LBLE)
                    let off = self.fetch16(bus) as i16;
                    let take = self.cond_eval(sub).unwrap_or(false);
                    if take {
                        self.r.pc = (self.r.pc as i32 + off as i32) as u16;
                        consumed = 6;
                    } else {
                        consumed = 5;
                    }
                    if trace {
                        println!(
                            "{:04X}: 10 {:02X} {:04X} LB{:02X} {}",
                            pc0,
                            sub,
                            off as u16,
                            sub,
                            if take { "taken" } else { "not" }
                        );
                    }
                } else if sub == 0x3F {
                    // SWI2 — vector at $FFF4/$FFF5. Same push-order fix
                    // as SWI: PC, U, Y, X, DP, B, A, CC (deepest first).
                    let ret = self.r.pc;
                    self.r.cc |= E;
                    self.push16(bus, self.r.pc);
                    self.push16(bus, self.r.u);
                    self.push16(bus, self.r.y);
                    self.push16(bus, self.r.x);
                    self.push8(bus, self.r.dp);
                    self.push8(bus, self.r.b);
                    self.push8(bus, self.r.a);
                    self.push8(bus, self.r.cc);
                    self.r.cc |= I | F;
                    let hi = bus.read8(0xFFF4) as u16;
                    let lo = bus.read8(0xFFF5) as u16;
                    self.r.pc = (hi << 8) | lo;
                    self.shadow_stack.push(CallFrame {
                        return_addr: ret,
                        call_site: pc0,
                        target: self.r.pc,
                        sp_at_call: self.r.s,
                        kind: CallKind::Swi(2),
                    });
                    consumed = 20;
                    if trace {
                        println!("{:04X}: 10 3F   SWI2", pc0);
                    }
                } else if sub == 0x83 {
                    // CMPD #imm16
                    let imm = self.fetch16(bus);
                    let d = ((self.r.a as u16) << 8) | (self.r.b as u16);
                    let _ = self.sub16_flags(d, imm);
                    consumed = 5;
                    if trace {
                        println!("{:04X}: 10 83 {:04X} CMPD #${:04X}", pc0, imm, imm);
                    }
                } else if sub == 0x93 {
                    // CMPD <dir
                    let zp = self.fetch8(bus);
                    let addr = self.ea_direct(zp);
                    let hi = bus.read8(addr) as u16;
                    let lo = bus.read8(addr.wrapping_add(1)) as u16;
                    let v = (hi << 8) | lo;
                    let d = ((self.r.a as u16) << 8) | (self.r.b as u16);
                    let _ = self.sub16_flags(d, v);
                    consumed = 7;
                    if trace {
                        println!("{:04X}: 10 93 {:02X} CMPD <${:02X}", pc0, zp, zp);
                    }
                } else if sub == 0xA3 {
                    // CMPD indexed
                    let (ea, addcyc, desc) = self.ea_indexed(bus);
                    let hi = bus.read8(ea) as u16;
                    let lo = bus.read8(ea.wrapping_add(1)) as u16;
                    let v = (hi << 8) | lo;
                    let d = ((self.r.a as u16) << 8) | (self.r.b as u16);
                    let _ = self.sub16_flags(d, v);
                    consumed = 7 + addcyc;
                    if trace {
                        println!("{:04X}: 10 A3 ..    CMPD {}", pc0, desc);
                    }
                } else if sub == 0xB3 {
                    // CMPD extended
                    let addr = self.ea_extended(bus);
                    let hi = bus.read8(addr) as u16;
                    let lo = bus.read8(addr.wrapping_add(1)) as u16;
                    let v = (hi << 8) | lo;
                    let d = ((self.r.a as u16) << 8) | (self.r.b as u16);
                    let _ = self.sub16_flags(d, v);
                    consumed = 8;
                    if trace {
                        println!("{:04X}: 10 B3 {:04X} CMPD ${:04X}", pc0, addr, addr);
                    }
                } else if sub == 0x8E {
                    // LDY #imm16
                    let imm = self.fetch16(bus);
                    self.r.y = imm;
                    self.set_nz16(self.r.y);
                    self.r.cc &= !V;
                    consumed = 4;
                    if trace {
                        println!("{:04X}: 10 8E {:04X} LDY #${:04X}", pc0, imm, imm);
                    }
                } else if sub == 0x9F {
                    // STY <dir
                    let zp = self.fetch8(bus);
                    let addr = self.ea_direct(zp);
                    bus.write8(addr, (self.r.y >> 8) as u8);
                    bus.write8(addr.wrapping_add(1), (self.r.y & 0xFF) as u8);
                    self.set_nz16(self.r.y);
                    self.r.cc &= !V;
                    consumed = 5;
                    if trace {
                        println!("{:04X}: 10 9F {:02X} STY <${:02X}", pc0, zp, zp);
                    }
                } else if sub == 0xAF {
                    // STY indexed
                    let (ea, addcyc, desc) = self.ea_indexed(bus);
                    bus.write8(ea, (self.r.y >> 8) as u8);
                    bus.write8(ea.wrapping_add(1), (self.r.y & 0xFF) as u8);
                    self.set_nz16(self.r.y);
                    self.r.cc &= !V;
                    consumed = 5 + addcyc;
                    if trace {
                        println!("{:04X}: 10 AF ..    STY {}", pc0, desc);
                    }
                } else if sub == 0x8C {
                    // CMPY #imm16
                    let imm = self.fetch16(bus);
                    let y = self.r.y;
                    let _ = self.sub16_flags(y, imm);
                    consumed = 5;
                    if trace {
                        println!("{:04X}: 10 8C {:04X} CMPY #${:04X}", pc0, imm, imm);
                    }
                } else if sub == 0x9E {
                    // LDY <dir
                    let zp = self.fetch8(bus);
                    let addr = self.ea_direct(zp);
                    let hi = bus.read8(addr) as u16;
                    let lo = bus.read8(addr.wrapping_add(1)) as u16;
                    self.r.y = (hi << 8) | lo;
                    self.set_nz16(self.r.y);
                    self.r.cc &= !V;
                    consumed = 5;
                    if trace {
                        println!("{:04X}: 10 9E {:02X} LDY <${:02X}", pc0, zp, zp);
                    }
                } else if sub == 0xAE {
                    // LDY indexed
                    let (ea, addcyc, desc) = self.ea_indexed(bus);
                    let hi = bus.read8(ea) as u16;
                    let lo = bus.read8(ea.wrapping_add(1)) as u16;
                    self.r.y = (hi << 8) | lo;
                    self.set_nz16(self.r.y);
                    self.r.cc &= !V;
                    consumed = 5 + addcyc;
                    if trace {
                        println!("{:04X}: 10 AE ..    LDY {}", pc0, desc);
                    }
                } else if sub == 0xBE {
                    // LDY extended
                    let addr = self.ea_extended(bus);
                    let hi = bus.read8(addr) as u16;
                    let lo = bus.read8(addr.wrapping_add(1)) as u16;
                    self.r.y = (hi << 8) | lo;
                    self.set_nz16(self.r.y);
                    self.r.cc &= !V;
                    consumed = 6;
                    if trace {
                        println!("{:04X}: 10 BE {:04X} LDY ${:04X}", pc0, addr, addr);
                    }
                } else if sub == 0xBF {
                    // STY extended
                    let addr = self.ea_extended(bus);
                    bus.write8(addr, (self.r.y >> 8) as u8);
                    bus.write8(addr.wrapping_add(1), (self.r.y & 0xFF) as u8);
                    self.set_nz16(self.r.y);
                    self.r.cc &= !V;
                    consumed = 6;
                    if trace {
                        println!("{:04X}: 10 BF {:04X} STY ${:04X}", pc0, addr, addr);
                    }
                } else if sub == 0x9C {
                    // CMPY <dir
                    let zp = self.fetch8(bus);
                    let addr = self.ea_direct(zp);
                    let hi = bus.read8(addr) as u16;
                    let lo = bus.read8(addr.wrapping_add(1)) as u16;
                    let v = (hi << 8) | lo;
                    let _ = self.sub16_flags(self.r.y, v);
                    consumed = 5;
                    if trace {
                        println!("{:04X}: 10 9C {:02X} CMPY <${:02X}", pc0, zp, zp);
                    }
                } else if sub == 0xAC {
                    // CMPY indexed
                    let (ea, addcyc, desc) = self.ea_indexed(bus);
                    let hi = bus.read8(ea) as u16;
                    let lo = bus.read8(ea.wrapping_add(1)) as u16;
                    let v = (hi << 8) | lo;
                    let _ = self.sub16_flags(self.r.y, v);
                    consumed = 5 + addcyc;
                    if trace {
                        println!("{:04X}: 10 AC ..    CMPY {}", pc0, desc);
                    }
                } else if sub == 0xBC {
                    // CMPY extended
                    let addr = self.ea_extended(bus);
                    let hi = bus.read8(addr) as u16;
                    let lo = bus.read8(addr.wrapping_add(1)) as u16;
                    let v = (hi << 8) | lo;
                    let _ = self.sub16_flags(self.r.y, v);
                    consumed = 6;
                    if trace {
                        println!("{:04X}: 10 BC {:04X} CMPY ${:04X}", pc0, addr, addr);
                    }
                } else if sub == 0xCE {
                    // LDS #imm16
                    let imm = self.fetch16(bus);
                    self.r.s = imm;
                    self.set_nz16(self.r.s);
                    self.r.cc &= !V;
                    consumed = 4;
                    if trace {
                        println!("{:04X}: 10 CE {:04X} LDS #${:04X}", pc0, imm, imm);
                    }
                } else if sub == 0xDE {
                    // LDS <dir
                    let zp = self.fetch8(bus);
                    let addr = self.ea_direct(zp);
                    let hi = bus.read8(addr) as u16;
                    let lo = bus.read8(addr.wrapping_add(1)) as u16;
                    self.r.s = (hi << 8) | lo;
                    self.set_nz16(self.r.s);
                    self.r.cc &= !V;
                    consumed = 5;
                    if trace {
                        println!("{:04X}: 10 DE {:02X} LDS <${:02X}", pc0, zp, zp);
                    }
                } else if sub == 0xEE {
                    // LDS indexed
                    let (ea, addcyc, desc) = self.ea_indexed(bus);
                    let hi = bus.read8(ea) as u16;
                    let lo = bus.read8(ea.wrapping_add(1)) as u16;
                    self.r.s = (hi << 8) | lo;
                    self.set_nz16(self.r.s);
                    self.r.cc &= !V;
                    consumed = 5 + addcyc;
                    if trace {
                        println!("{:04X}: 10 EE ..    LDS {}", pc0, desc);
                    }
                } else if sub == 0xFE {
                    // LDS extended
                    let addr = self.ea_extended(bus);
                    let hi = bus.read8(addr) as u16;
                    let lo = bus.read8(addr.wrapping_add(1)) as u16;
                    self.r.s = (hi << 8) | lo;
                    self.set_nz16(self.r.s);
                    self.r.cc &= !V;
                    consumed = 6;
                    if trace {
                        println!("{:04X}: 10 FE {:04X} LDS ${:04X}", pc0, addr, addr);
                    }
                } else if sub == 0xDF {
                    // STS <dir
                    let zp = self.fetch8(bus);
                    let addr = self.ea_direct(zp);
                    bus.write8(addr, (self.r.s >> 8) as u8);
                    bus.write8(addr.wrapping_add(1), (self.r.s & 0xFF) as u8);
                    self.set_nz16(self.r.s);
                    self.r.cc &= !V;
                    consumed = 5;
                    if trace {
                        println!("{:04X}: 10 DF {:02X} STS <${:02X}", pc0, zp, zp);
                    }
                } else if sub == 0xEF {
                    // STS indexed
                    let (ea, addcyc, desc) = self.ea_indexed(bus);
                    bus.write8(ea, (self.r.s >> 8) as u8);
                    bus.write8(ea.wrapping_add(1), (self.r.s & 0xFF) as u8);
                    self.set_nz16(self.r.s);
                    self.r.cc &= !V;
                    consumed = 5 + addcyc;
                    if trace {
                        println!("{:04X}: 10 EF ..    STS {}", pc0, desc);
                    }
                } else if sub == 0xFF {
                    // STS extended
                    let addr = self.ea_extended(bus);
                    bus.write8(addr, (self.r.s >> 8) as u8);
                    bus.write8(addr.wrapping_add(1), (self.r.s & 0xFF) as u8);
                    self.set_nz16(self.r.s);
                    self.r.cc &= !V;
                    consumed = 6;
                    if trace {
                        println!("{:04X}: 10 FF {:04X} STS ${:04X}", pc0, addr, addr);
                    }
                } else if let Some(taken) = self.cond_eval(sub) {
                    let off = self.fetch16(bus) as i16;
                    if taken {
                        let newpc = (self.r.pc as i32 + off as i32) as u16;
                        self.r.pc = newpc;
                        consumed = 5; // taken
                        if trace {
                            println!(
                                "{:04X}: 10 {:02X} {:04X} LBR taken ${:04X}",
                                pc0, sub, off as u16, newpc
                            );
                        }
                    } else {
                        consumed = 4; // not taken
                        if trace {
                            println!("{:04X}: 10 {:02X} {:04X} LBR not", pc0, sub, off as u16);
                        }
                    }
                } else {
                    if trace {
                        println!("{:04X}: 10 {:02X}   UNIMPL-PFX", pc0, sub);
                    }
                    consumed = 1;
                }
            }
            0x32 => {
                // LEAS indexed — does NOT update CC per 6809 ISA
                let (ea, addcyc, _desc) = self.ea_indexed(bus);
                self.r.s = ea;
                consumed = 4 + addcyc;
                if trace {
                    println!("{:04X}: 32 ..    LEAS", pc0);
                }
            }
            0x33 => {
                // LEAU indexed — does NOT update CC per 6809 ISA
                let (ea, addcyc, _desc) = self.ea_indexed(bus);
                self.r.u = ea;
                consumed = 4 + addcyc;
                if trace {
                    println!("{:04X}: 33 ..    LEAU", pc0);
                }
            }
            0x3A => {
                // ABX — X = X + (unsigned) B.  No flags affected.
                self.r.x = self.r.x.wrapping_add(self.r.b as u16);
                consumed = 3;
                if trace {
                    println!("{:04X}: 3A        ABX", pc0);
                }
            }
            0xCE => {
                // LDU #imm16
                let imm = self.fetch16(bus);
                self.r.u = imm;
                self.set_nz16(self.r.u);
                self.r.cc &= !V;
                consumed = 3;
                if trace {
                    println!("{:04X}: CE {:04X} LDU #${:04X}", pc0, imm, imm);
                }
            }
            0xDE => {
                // LDU <dir
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                let hi = bus.read8(addr) as u16;
                let lo = bus.read8(addr.wrapping_add(1)) as u16;
                self.r.u = (hi << 8) | lo;
                self.set_nz16(self.r.u);
                self.r.cc &= !V;
                consumed = 4;
                if trace {
                    println!("{:04X}: DE {:02X} LDU <${:02X}", pc0, zp, zp);
                }
            }
            0xEE => {
                // LDU indexed
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                let hi = bus.read8(ea) as u16;
                let lo = bus.read8(ea.wrapping_add(1)) as u16;
                self.r.u = (hi << 8) | lo;
                self.set_nz16(self.r.u);
                self.r.cc &= !V;
                consumed = 5 + addcyc;
                if trace {
                    println!("{:04X}: EE ..    LDU {}", pc0, desc);
                }
            }
            0xFE => {
                // LDU extended
                let addr = self.ea_extended(bus);
                let hi = bus.read8(addr) as u16;
                let lo = bus.read8(addr.wrapping_add(1)) as u16;
                self.r.u = (hi << 8) | lo;
                self.set_nz16(self.r.u);
                self.r.cc &= !V;
                consumed = 6;
                if trace {
                    println!("{:04X}: FE {:04X} LDU ${:04X}", pc0, addr, addr);
                }
            }
            0xDF => {
                // STU <dir
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                bus.write8(addr, (self.r.u >> 8) as u8);
                bus.write8(addr.wrapping_add(1), (self.r.u & 0xFF) as u8);
                self.set_nz16(self.r.u);
                self.r.cc &= !V;
                consumed = 4;
                if trace {
                    println!("{:04X}: DF {:02X} STU <${:02X}", pc0, zp, zp);
                }
            }
            0xEF => {
                // STU indexed
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                bus.write8(ea, (self.r.u >> 8) as u8);
                bus.write8(ea.wrapping_add(1), (self.r.u & 0xFF) as u8);
                self.set_nz16(self.r.u);
                self.r.cc &= !V;
                consumed = 5 + addcyc;
                if trace {
                    println!("{:04X}: EF ..    STU {}", pc0, desc);
                }
            }
            0xFF => {
                // STU extended
                let addr = self.ea_extended(bus);
                bus.write8(addr, (self.r.u >> 8) as u8);
                bus.write8(addr.wrapping_add(1), (self.r.u & 0xFF) as u8);
                self.set_nz16(self.r.u);
                self.r.cc &= !V;
                consumed = 6;
                if trace {
                    println!("{:04X}: FF {:04X} STU ${:04X}", pc0, addr, addr);
                }
            }
            // ----- LDD family ($CC/$DC/$EC/$FC) -------------------------------
            0xCC => {
                // LDD #imm16
                let imm = self.fetch16(bus);
                self.r.a = (imm >> 8) as u8;
                self.r.b = (imm & 0xFF) as u8;
                self.set_nz16(imm);
                self.r.cc &= !V;
                consumed = 3;
                if trace {
                    println!("{:04X}: CC {:04X} LDD #${:04X}", pc0, imm, imm);
                }
            }
            0xDC => {
                // LDD <dir
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                let hi = bus.read8(addr);
                let lo = bus.read8(addr.wrapping_add(1));
                self.r.a = hi;
                self.r.b = lo;
                self.set_nz16(((hi as u16) << 8) | (lo as u16));
                self.r.cc &= !V;
                consumed = 5;
                if trace {
                    println!("{:04X}: DC {:02X}   LDD <${:02X}", pc0, zp, zp);
                }
            }
            0xEC => {
                // LDD indexed
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                let hi = bus.read8(ea);
                let lo = bus.read8(ea.wrapping_add(1));
                self.r.a = hi;
                self.r.b = lo;
                self.set_nz16(((hi as u16) << 8) | (lo as u16));
                self.r.cc &= !V;
                consumed = 5 + addcyc;
                if trace {
                    println!("{:04X}: EC ..    LDD {}", pc0, desc);
                }
            }
            0xFC => {
                // LDD extended
                let addr = self.ea_extended(bus);
                let hi = bus.read8(addr);
                let lo = bus.read8(addr.wrapping_add(1));
                self.r.a = hi;
                self.r.b = lo;
                self.set_nz16(((hi as u16) << 8) | (lo as u16));
                self.r.cc &= !V;
                consumed = 6;
                if trace {
                    println!("{:04X}: FC {:04X} LDD ${:04X}", pc0, addr, addr);
                }
            }
            // ----- STD family ($DD/$ED/$FD) -----------------------------------
            0xDD => {
                // STD <dir
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                bus.write8(addr, self.r.a);
                bus.write8(addr.wrapping_add(1), self.r.b);
                self.set_nz16(((self.r.a as u16) << 8) | (self.r.b as u16));
                self.r.cc &= !V;
                consumed = 5;
                if trace {
                    println!("{:04X}: DD {:02X}   STD <${:02X}", pc0, zp, zp);
                }
            }
            0xED => {
                // STD indexed
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                bus.write8(ea, self.r.a);
                bus.write8(ea.wrapping_add(1), self.r.b);
                self.set_nz16(((self.r.a as u16) << 8) | (self.r.b as u16));
                self.r.cc &= !V;
                consumed = 5 + addcyc;
                if trace {
                    println!("{:04X}: ED ..    STD {}", pc0, desc);
                }
            }
            0xFD => {
                // STD extended
                let addr = self.ea_extended(bus);
                bus.write8(addr, self.r.a);
                bus.write8(addr.wrapping_add(1), self.r.b);
                self.set_nz16(((self.r.a as u16) << 8) | (self.r.b as u16));
                self.r.cc &= !V;
                consumed = 6;
                if trace {
                    println!("{:04X}: FD {:04X} STD ${:04X}", pc0, addr, addr);
                }
            }
            // ----- LDX family main-table forms ($9E/$BE) ----------------------
            0x9E => {
                // LDX <dir
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                let hi = bus.read8(addr) as u16;
                let lo = bus.read8(addr.wrapping_add(1)) as u16;
                self.r.x = (hi << 8) | lo;
                self.set_nz16(self.r.x);
                self.r.cc &= !V;
                consumed = 5;
                if trace {
                    println!("{:04X}: 9E {:02X}   LDX <${:02X}", pc0, zp, zp);
                }
            }
            0xBE => {
                // LDX extended
                let addr = self.ea_extended(bus);
                let hi = bus.read8(addr) as u16;
                let lo = bus.read8(addr.wrapping_add(1)) as u16;
                self.r.x = (hi << 8) | lo;
                self.set_nz16(self.r.x);
                self.r.cc &= !V;
                consumed = 6;
                if trace {
                    println!("{:04X}: BE {:04X} LDX ${:04X}", pc0, addr, addr);
                }
            }
            0x9F => {
                // STX <dir
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                bus.write8(addr, (self.r.x >> 8) as u8);
                bus.write8(addr.wrapping_add(1), (self.r.x & 0xFF) as u8);
                self.set_nz16(self.r.x);
                self.r.cc &= !V;
                consumed = 5;
                if trace {
                    println!("{:04X}: 9F {:02X}   STX <${:02X}", pc0, zp, zp);
                }
            }
            0xBF => {
                // STX extended
                let addr = self.ea_extended(bus);
                bus.write8(addr, (self.r.x >> 8) as u8);
                bus.write8(addr.wrapping_add(1), (self.r.x & 0xFF) as u8);
                self.set_nz16(self.r.x);
                self.r.cc &= !V;
                consumed = 6;
                if trace {
                    println!("{:04X}: BF {:04X} STX ${:04X}", pc0, addr, addr);
                }
            }
            0x11 => {
                // SWI3 or extended prefix
                let sub = self.fetch8(bus);
                if sub == 0x3F {
                    // SWI3 — vector at $FFF2/$FFF3. Same push-order fix
                    // as SWI: PC, U, Y, X, DP, B, A, CC (deepest first).
                    let ret = self.r.pc;
                    self.r.cc |= E;
                    self.push16(bus, self.r.pc);
                    self.push16(bus, self.r.u);
                    self.push16(bus, self.r.y);
                    self.push16(bus, self.r.x);
                    self.push8(bus, self.r.dp);
                    self.push8(bus, self.r.b);
                    self.push8(bus, self.r.a);
                    self.push8(bus, self.r.cc);
                    self.r.cc |= I | F;
                    let hi = bus.read8(0xFFF2) as u16;
                    let lo = bus.read8(0xFFF3) as u16;
                    self.r.pc = (hi << 8) | lo;
                    self.shadow_stack.push(CallFrame {
                        return_addr: ret,
                        call_site: pc0,
                        target: self.r.pc,
                        sp_at_call: self.r.s,
                        kind: CallKind::Swi(3),
                    });
                    consumed = 20;
                    if trace {
                        println!("{:04X}: 11 3F   SWI3", pc0);
                    }
                } else if sub == 0x83 {
                    // CMPU #imm16
                    let imm = self.fetch16(bus);
                    let _ = self.sub16_flags(self.r.u, imm);
                    consumed = 5;
                    if trace {
                        println!("{:04X}: 11 83 {:04X} CMPU #${:04X}", pc0, imm, imm);
                    }
                } else if sub == 0x8C {
                    // CMPS #imm16
                    let imm = self.fetch16(bus);
                    let _ = self.sub16_flags(self.r.s, imm);
                    consumed = 5;
                    if trace {
                        println!("{:04X}: 11 8C {:04X} CMPS #${:04X}", pc0, imm, imm);
                    }
                } else if sub == 0x93 {
                    // CMPU <dir
                    let zp = self.fetch8(bus);
                    let addr = self.ea_direct(zp);
                    let hi = bus.read8(addr) as u16;
                    let lo = bus.read8(addr.wrapping_add(1)) as u16;
                    let v = (hi << 8) | lo;
                    let _ = self.sub16_flags(self.r.u, v);
                    consumed = 5;
                    if trace {
                        println!("{:04X}: 11 93 {:02X} CMPU <${:02X}", pc0, zp, zp);
                    }
                } else if sub == 0x9C {
                    // CMPS <dir
                    let zp = self.fetch8(bus);
                    let addr = self.ea_direct(zp);
                    let hi = bus.read8(addr) as u16;
                    let lo = bus.read8(addr.wrapping_add(1)) as u16;
                    let v = (hi << 8) | lo;
                    let _ = self.sub16_flags(self.r.s, v);
                    consumed = 5;
                    if trace {
                        println!("{:04X}: 11 9C {:02X} CMPS <${:02X}", pc0, zp, zp);
                    }
                } else if sub == 0xA3 {
                    // CMPU indexed
                    let (ea, addcyc, desc) = self.ea_indexed(bus);
                    let hi = bus.read8(ea) as u16;
                    let lo = bus.read8(ea.wrapping_add(1)) as u16;
                    let v = (hi << 8) | lo;
                    let _ = self.sub16_flags(self.r.u, v);
                    consumed = 5 + addcyc;
                    if trace {
                        println!("{:04X}: 11 A3 ..    CMPU {}", pc0, desc);
                    }
                } else if sub == 0xAC {
                    // CMPS indexed
                    let (ea, addcyc, desc) = self.ea_indexed(bus);
                    let hi = bus.read8(ea) as u16;
                    let lo = bus.read8(ea.wrapping_add(1)) as u16;
                    let v = (hi << 8) | lo;
                    let _ = self.sub16_flags(self.r.s, v);
                    consumed = 5 + addcyc;
                    if trace {
                        println!("{:04X}: 11 AC ..    CMPS {}", pc0, desc);
                    }
                } else if sub == 0xB3 {
                    // CMPU extended
                    let addr = self.ea_extended(bus);
                    let hi = bus.read8(addr) as u16;
                    let lo = bus.read8(addr.wrapping_add(1)) as u16;
                    let v = (hi << 8) | lo;
                    let _ = self.sub16_flags(self.r.u, v);
                    consumed = 6;
                    if trace {
                        println!("{:04X}: 11 B3 {:04X} CMPU ${:04X}", pc0, addr, addr);
                    }
                } else if sub == 0xBC {
                    // CMPS extended
                    let addr = self.ea_extended(bus);
                    let hi = bus.read8(addr) as u16;
                    let lo = bus.read8(addr.wrapping_add(1)) as u16;
                    let v = (hi << 8) | lo;
                    let _ = self.sub16_flags(self.r.s, v);
                    consumed = 6;
                    if trace {
                        println!("{:04X}: 11 BC {:04X} CMPS ${:04X}", pc0, addr, addr);
                    }
                } else {
                    consumed = 1;
                    if trace {
                        println!("{:04X}: 11 {:02X}   UNIMPL-PFX", pc0, sub);
                    }
                }
            }
            0x30 => {
                // LEAX indexed
                let (ea, addcyc, _desc) = self.ea_indexed(bus);
                self.r.x = ea;
                // LEA family only affects Z flag
                self.r.cc = (self.r.cc & !Z) | (if self.r.x == 0 { Z } else { 0 });
                consumed = 4 + addcyc;
                if trace {
                    println!("{:04X}: 30 ..    LEAX", pc0);
                }
            }
            0x31 => {
                // LEAY indexed
                let (ea, addcyc, _desc) = self.ea_indexed(bus);
                self.r.y = ea;
                self.r.cc = (self.r.cc & !Z) | (if self.r.y == 0 { Z } else { 0 });
                consumed = 4 + addcyc;
                if trace {
                    println!("{:04X}: 31 ..    LEAY", pc0);
                }
            }
            0x8C => {
                // CMPX #imm16
                let imm = self.fetch16(bus);
                let x = self.r.x;
                let _ = self.sub16_flags(x, imm);
                consumed = 4;
                if trace {
                    println!("{:04X}: 8C {:04X} CMPX #${:04X}", pc0, imm, imm);
                }
            }
            0x9C => {
                // CMPX <dir
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                let hi = bus.read8(addr) as u16;
                let lo = bus.read8(addr.wrapping_add(1)) as u16;
                let v = (hi << 8) | lo;
                let _ = self.sub16_flags(self.r.x, v);
                consumed = 4;
                if trace {
                    println!("{:04X}: 9C {:02X} CMPX <${:02X}", pc0, zp, zp);
                }
            }
            0xAC => {
                // CMPX indexed
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                let hi = bus.read8(ea) as u16;
                let lo = bus.read8(ea.wrapping_add(1)) as u16;
                let v = (hi << 8) | lo;
                let _ = self.sub16_flags(self.r.x, v);
                consumed = 5 + addcyc;
                if trace {
                    println!("{:04X}: AC ..    CMPX {}", pc0, desc);
                }
            }
            0xBC => {
                // CMPX extended
                let addr = self.ea_extended(bus);
                let hi = bus.read8(addr) as u16;
                let lo = bus.read8(addr.wrapping_add(1)) as u16;
                let v = (hi << 8) | lo;
                let _ = self.sub16_flags(self.r.x, v);
                consumed = 5;
                if trace {
                    println!("{:04X}: BC {:04X} CMPX ${:04X}", pc0, addr, addr);
                }
            }
            // duplicate 0x11 handler removed; handled above
            0x1E => {
                // EXG
                let pb = self.fetch8(bus);
                let src = pb >> 4;
                let dst = pb & 0x0F;
                let is16 = (src & 0x08) == 0 && (dst & 0x08) == 0;
                let is8 = (src & 0x08) != 0 && (dst & 0x08) != 0;
                if is16 {
                    if let (Some(vs), Some(vd)) =
                        (self.get_reg16_by_code(src), self.get_reg16_by_code(dst))
                    {
                        self.set_reg16_by_code(src, vd);
                        self.set_reg16_by_code(dst, vs);
                    }
                } else if is8 {
                    if let (Some(vs), Some(vd)) =
                        (self.get_reg8_by_code(src), self.get_reg8_by_code(dst))
                    {
                        self.set_reg8_by_code(src, vd);
                        self.set_reg8_by_code(dst, vs);
                    }
                }
                consumed = 6;
                if trace {
                    println!("{:04X}: 1E {:02X}   EXG", pc0, pb);
                }
            }
            0x1F => {
                // TFR
                let pb = self.fetch8(bus);
                let src = pb >> 4;
                let dst = pb & 0x0F;
                let is16 = (src & 0x08) == 0 && (dst & 0x08) == 0;
                let is8 = (src & 0x08) != 0 && (dst & 0x08) != 0;
                if is16 {
                    if let Some(vs) = self.get_reg16_by_code(src) {
                        self.set_reg16_by_code(dst, vs);
                    }
                } else if is8 {
                    if let Some(vs) = self.get_reg8_by_code(src) {
                        self.set_reg8_by_code(dst, vs);
                    }
                }
                consumed = 6;
                if trace {
                    println!("{:04X}: 1F {:02X}   TFR", pc0, pb);
                }
            }
            0x12 => {
                // NOP
                consumed = 2;
                if trace {
                    println!("{:04X}: 12        NOP", pc0);
                }
            }
            0x86 => {
                // LDA #imm
                let imm = self.fetch8(bus);
                self.r.a = imm;
                self.set_nz8(self.r.a);
                self.r.cc &= !V; // V cleared, C unaffected
                consumed = 2;
                if trace {
                    println!("{:04X}: 86 {:02X}   LDA #${:02X}", pc0, imm, imm);
                }
            }
            0x96 => {
                // LDA direct
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                self.r.a = bus.read8(addr);
                self.set_nz8(self.r.a);
                self.r.cc &= !V;
                consumed = 4;
                if trace {
                    println!("{:04X}: 96 {:02X}   LDA ${:02X}", pc0, zp, zp);
                }
            }
            0xB6 => {
                // LDA extended
                let addr = self.ea_extended(bus);
                self.r.a = bus.read8(addr);
                self.set_nz8(self.r.a);
                self.r.cc &= !V;
                consumed = 5;
                if trace {
                    println!("{:04X}: B6 {:04X} LDA ${:04X}", pc0, addr, addr);
                }
            }
            0x97 => {
                // STA direct
                let zp = self.fetch8(bus);
                let addr = ((self.r.dp as u16) << 8) | (zp as u16);
                bus.write8(addr, self.r.a);
                self.set_nz8(self.r.a);
                self.r.cc &= !(V);
                consumed = 4;
                if trace {
                    println!("{:04X}: 97 {:02X}   STA ${:02X}", pc0, zp, zp);
                }
            }
            0xB7 => {
                // STA extended
                let addr = self.ea_extended(bus);
                bus.write8(addr, self.r.a);
                self.set_nz8(self.r.a);
                self.r.cc &= !V;
                consumed = 5;
                if trace {
                    println!("{:04X}: B7 {:04X} STA ${:04X}", pc0, addr, addr);
                }
            }
            0xA6 => {
                // LDA indexed
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                self.r.a = v;
                self.set_nz8(v);
                self.r.cc &= !V;
                consumed = 4 + addcyc;
                if trace {
                    println!("{:04X}: A6 ..    LDA {} -> {:04X}", pc0, desc, ea);
                }
            }
            0xA7 => {
                // STA indexed
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                bus.write8(ea, self.r.a);
                self.set_nz8(self.r.a);
                self.r.cc &= !V;
                consumed = 4 + addcyc;
                if trace {
                    println!("{:04X}: A7 ..    STA {} -> {:04X}", pc0, desc, ea);
                }
            }
            0x7E => {
                // JMP extended
                let addr = self.fetch16(bus);
                self.r.pc = addr;
                consumed = 3;
                if trace {
                    println!("{:04X}: 7E {:04X} JMP ${:04X}", pc0, addr, addr);
                }
            }
            0x0E => {
                // JMP direct
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                self.r.pc = addr;
                consumed = 3;
                if trace {
                    println!("{:04X}: 0E {:02X}   JMP <${:02X}", pc0, zp, zp);
                }
            }
            0x6E => {
                // JMP indexed (covers ,Y  [,Y]  n,X  etc.)
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                self.r.pc = ea;
                consumed = 3 + addcyc;
                if trace {
                    println!("{:04X}: 6E ..    JMP {}", pc0, desc);
                }
            }
            0x9D => {
                // JSR direct
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                let ret = self.r.pc;
                self.r.s = self.r.s.wrapping_sub(1);
                bus.write8(self.r.s, (ret & 0x00FF) as u8);
                self.r.s = self.r.s.wrapping_sub(1);
                bus.write8(self.r.s, (ret >> 8) as u8);
                self.r.pc = addr;
                self.shadow_stack.push(CallFrame {
                    return_addr: ret,
                    call_site: pc0,
                    target: self.r.pc,
                    sp_at_call: self.r.s,
                    kind: CallKind::Jsr,
                });
                consumed = 7;
                if trace {
                    println!("{:04X}: 9D {:02X}   JSR <${:02X}", pc0, zp, zp);
                }
            }
            0xAD => {
                // JSR indexed
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                let ret = self.r.pc;
                self.r.s = self.r.s.wrapping_sub(1);
                bus.write8(self.r.s, (ret & 0x00FF) as u8);
                self.r.s = self.r.s.wrapping_sub(1);
                bus.write8(self.r.s, (ret >> 8) as u8);
                self.r.pc = ea;
                self.shadow_stack.push(CallFrame {
                    return_addr: ret,
                    call_site: pc0,
                    target: self.r.pc,
                    sp_at_call: self.r.s,
                    kind: CallKind::Jsr,
                });
                consumed = 7 + addcyc;
                if trace {
                    println!("{:04X}: AD ..    JSR {}", pc0, desc);
                }
            }
            0x20 => {
                // BRA relative
                let off = self.fetch8(bus) as i8 as i16;
                let newpc = (self.r.pc as i16).wrapping_add(off) as u16;
                self.r.pc = newpc;
                consumed = 3;
                if trace {
                    println!(
                        "{:04X}: 20 {:02X}   BRA ${:04X}",
                        pc0,
                        (off as i8) as u8,
                        newpc
                    );
                }
            }
            0x16 => {
                // LBRA relative 16
                let off = self.fetch16(bus) as i16;
                let newpc = (self.r.pc as i32 + off as i32) as u16;
                self.r.pc = newpc;
                consumed = 5;
                if trace {
                    println!("{:04X}: 16 {:04X} LBRA ${:04X}", pc0, off as u16, newpc);
                }
            }
            0x26 => {
                // BNE relative
                let offb = self.fetch8(bus);
                let zero = (self.r.cc & Z) != 0;
                if !zero {
                    let off = (offb as i8) as i16;
                    self.r.pc = (self.r.pc as i16).wrapping_add(off) as u16;
                    consumed = 3;
                } else {
                    consumed = 2;
                }
                if trace {
                    println!(
                        "{:04X}: 26 {:02X}   BNE {}",
                        pc0,
                        offb,
                        if !zero { "taken" } else { "not" }
                    );
                }
            }
            0x27 => {
                // BEQ relative
                let offb = self.fetch8(bus);
                let zero = (self.r.cc & Z) != 0;
                if zero {
                    let off = (offb as i8) as i16;
                    self.r.pc = (self.r.pc as i16).wrapping_add(off) as u16;
                    consumed = 3;
                } else {
                    consumed = 2;
                }
                if trace {
                    println!(
                        "{:04X}: 27 {:02X}   BEQ {}",
                        pc0,
                        offb,
                        if zero { "taken" } else { "not" }
                    );
                }
            }
            0x21 => {
                // BRN (never)
                let offb = self.fetch8(bus);
                consumed = 2;
                if trace {
                    println!("{:04X}: 21 {:02X}   BRN not", pc0, offb);
                }
            }
            0x22 => {
                // BHI
                let offb = self.fetch8(bus);
                let take = self.cond_eval(0x22).unwrap();
                if take {
                    let off = (offb as i8) as i16;
                    self.r.pc = (self.r.pc as i16).wrapping_add(off) as u16;
                    consumed = 3;
                } else {
                    consumed = 2;
                }
                if trace {
                    println!(
                        "{:04X}: 22 {:02X}   BHI {}",
                        pc0,
                        offb,
                        if take { "taken" } else { "not" }
                    );
                }
            }
            0x23 => {
                // BLS
                let offb = self.fetch8(bus);
                let take = self.cond_eval(0x23).unwrap();
                if take {
                    let off = (offb as i8) as i16;
                    self.r.pc = (self.r.pc as i16).wrapping_add(off) as u16;
                    consumed = 3;
                } else {
                    consumed = 2;
                }
                if trace {
                    println!(
                        "{:04X}: 23 {:02X}   BLS {}",
                        pc0,
                        offb,
                        if take { "taken" } else { "not" }
                    );
                }
            }
            0x24 => {
                // BCC/BHS
                let offb = self.fetch8(bus);
                let take = self.cond_eval(0x24).unwrap();
                if take {
                    let off = (offb as i8) as i16;
                    self.r.pc = (self.r.pc as i16).wrapping_add(off) as u16;
                    consumed = 3;
                } else {
                    consumed = 2;
                }
                if trace {
                    println!(
                        "{:04X}: 24 {:02X}   BCC {}",
                        pc0,
                        offb,
                        if take { "taken" } else { "not" }
                    );
                }
            }
            0x25 => {
                // BCS/BLO
                let offb = self.fetch8(bus);
                let take = self.cond_eval(0x25).unwrap();
                if take {
                    let off = (offb as i8) as i16;
                    self.r.pc = (self.r.pc as i16).wrapping_add(off) as u16;
                    consumed = 3;
                } else {
                    consumed = 2;
                }
                if trace {
                    println!(
                        "{:04X}: 25 {:02X}   BCS {}",
                        pc0,
                        offb,
                        if take { "taken" } else { "not" }
                    );
                }
            }
            0x28 => {
                // BVC
                let offb = self.fetch8(bus);
                let take = self.cond_eval(0x28).unwrap();
                if take {
                    let off = (offb as i8) as i16;
                    self.r.pc = (self.r.pc as i16).wrapping_add(off) as u16;
                    consumed = 3;
                } else {
                    consumed = 2;
                }
                if trace {
                    println!(
                        "{:04X}: 28 {:02X}   BVC {}",
                        pc0,
                        offb,
                        if take { "taken" } else { "not" }
                    );
                }
            }
            0x29 => {
                // BVS
                let offb = self.fetch8(bus);
                let take = self.cond_eval(0x29).unwrap();
                if take {
                    let off = (offb as i8) as i16;
                    self.r.pc = (self.r.pc as i16).wrapping_add(off) as u16;
                    consumed = 3;
                } else {
                    consumed = 2;
                }
                if trace {
                    println!(
                        "{:04X}: 29 {:02X}   BVS {}",
                        pc0,
                        offb,
                        if take { "taken" } else { "not" }
                    );
                }
            }
            0x2A => {
                // BPL
                let offb = self.fetch8(bus);
                let take = self.cond_eval(0x2A).unwrap();
                if take {
                    let off = (offb as i8) as i16;
                    self.r.pc = (self.r.pc as i16).wrapping_add(off) as u16;
                    consumed = 3;
                } else {
                    consumed = 2;
                }
                if trace {
                    println!(
                        "{:04X}: 2A {:02X}   BPL {}",
                        pc0,
                        offb,
                        if take { "taken" } else { "not" }
                    );
                }
            }
            0x2B => {
                // BMI
                let offb = self.fetch8(bus);
                let take = self.cond_eval(0x2B).unwrap();
                if take {
                    let off = (offb as i8) as i16;
                    self.r.pc = (self.r.pc as i16).wrapping_add(off) as u16;
                    consumed = 3;
                } else {
                    consumed = 2;
                }
                if trace {
                    println!(
                        "{:04X}: 2B {:02X}   BMI {}",
                        pc0,
                        offb,
                        if take { "taken" } else { "not" }
                    );
                }
            }
            0x2C => {
                // BGE
                let offb = self.fetch8(bus);
                let take = self.cond_eval(0x2C).unwrap();
                if take {
                    let off = (offb as i8) as i16;
                    self.r.pc = (self.r.pc as i16).wrapping_add(off) as u16;
                    consumed = 3;
                } else {
                    consumed = 2;
                }
                if trace {
                    println!(
                        "{:04X}: 2C {:02X}   BGE {}",
                        pc0,
                        offb,
                        if take { "taken" } else { "not" }
                    );
                }
            }
            0x2D => {
                // BLT
                let offb = self.fetch8(bus);
                let take = self.cond_eval(0x2D).unwrap();
                if take {
                    let off = (offb as i8) as i16;
                    self.r.pc = (self.r.pc as i16).wrapping_add(off) as u16;
                    consumed = 3;
                } else {
                    consumed = 2;
                }
                if trace {
                    println!(
                        "{:04X}: 2D {:02X}   BLT {}",
                        pc0,
                        offb,
                        if take { "taken" } else { "not" }
                    );
                }
            }
            0x2E => {
                // BGT
                let offb = self.fetch8(bus);
                let take = self.cond_eval(0x2E).unwrap();
                if take {
                    let off = (offb as i8) as i16;
                    self.r.pc = (self.r.pc as i16).wrapping_add(off) as u16;
                    consumed = 3;
                } else {
                    consumed = 2;
                }
                if trace {
                    println!(
                        "{:04X}: 2E {:02X}   BGT {}",
                        pc0,
                        offb,
                        if take { "taken" } else { "not" }
                    );
                }
            }
            0x2F => {
                // BLE
                let offb = self.fetch8(bus);
                let take = self.cond_eval(0x2F).unwrap();
                if take {
                    let off = (offb as i8) as i16;
                    self.r.pc = (self.r.pc as i16).wrapping_add(off) as u16;
                    consumed = 3;
                } else {
                    consumed = 2;
                }
                if trace {
                    println!(
                        "{:04X}: 2F {:02X}   BLE {}",
                        pc0,
                        offb,
                        if take { "taken" } else { "not" }
                    );
                }
            }
            0x17 => {
                // LBSR relative 16
                let off = self.fetch16(bus) as i16;
                let ret = self.r.pc;
                self.r.s = self.r.s.wrapping_sub(1);
                bus.write8(self.r.s, (ret & 0x00FF) as u8);
                self.r.s = self.r.s.wrapping_sub(1);
                bus.write8(self.r.s, (ret >> 8) as u8);
                self.r.pc = (self.r.pc as i32 + off as i32) as u16;
                self.shadow_stack.push(CallFrame {
                    return_addr: ret,
                    call_site: pc0,
                    target: self.r.pc,
                    sp_at_call: self.r.s,
                    kind: CallKind::Bsr,
                });
                consumed = 9;
                if trace {
                    println!("{:04X}: 17 {:04X} LBSR", pc0, off as u16);
                }
            }
            0x39 => {
                // RTS
                let hi = bus.read8(self.r.s);
                self.r.s = self.r.s.wrapping_add(1);
                let lo = bus.read8(self.r.s);
                self.r.s = self.r.s.wrapping_add(1);
                self.r.pc = ((hi as u16) << 8) | (lo as u16);
                // SP after the pop matches the recorded `sp_at_call`
                // for a normal call/return pair, which is the
                // condition pop_for_return uses to validate the frame.
                self.shadow_stack.pop_for_return(self.r.s);
                consumed = 5;
                if trace {
                    println!("{:04X}: 39        RTS", pc0);
                }
            }
            0x3B => {
                // RTI
                let new_cc = self.pull8(bus);
                let e_set = (new_cc & E) != 0;
                self.r.cc = new_cc;
                if e_set {
                    self.r.a = self.pull8(bus);
                    self.r.b = self.pull8(bus);
                    self.r.dp = self.pull8(bus);
                    self.r.x = self.pull16(bus);
                    self.r.y = self.pull16(bus);
                    self.r.u = self.pull16(bus);
                }
                self.r.pc = self.pull16(bus);
                // RTI may also peel off any stray non-interrupt frames
                // sitting on top (e.g., a BSR whose RTS the program
                // skipped via `puls pc`). pop_for_rti walks until it
                // finds a Swi/Irq/Firq/Nmi frame.
                self.shadow_stack.pop_for_rti();
                // 6809: full frame ~15, minimal ~12
                consumed = if e_set { 15 } else { 12 };
                if trace {
                    println!("{:04X}: 3B        RTI", pc0);
                }
            }
            0x13 => {
                // SYNC — halt until an interrupt is pending.  Without a full
                // interrupt-aware scheduler we treat this as a NOP so code
                // that uses SYNC as a timing hint still makes forward progress.
                consumed = 2;
                if trace {
                    println!("{:04X}: 13        SYNC", pc0);
                }
            }
            0x3C => {
                // CWAI #imm — AND CC with imm, set E, push full frame, halt.
                // Like SYNC, we fall through after setup instead of actually
                // waiting for an interrupt to return via RTI.
                let imm = self.fetch8(bus);
                self.r.cc &= imm;
                self.r.cc |= E;
                // Push full frame on S (PC, U, Y, X, DP, B, A, CC)
                self.push16(bus, self.r.pc);
                self.push16(bus, self.r.u);
                self.push16(bus, self.r.y);
                self.push16(bus, self.r.x);
                self.push8(bus, self.r.dp);
                self.push8(bus, self.r.b);
                self.push8(bus, self.r.a);
                self.push8(bus, self.r.cc);
                consumed = 20;
                if trace {
                    println!("{:04X}: 3C {:02X}    CWAI #${:02X}", pc0, imm, imm);
                }
            }
            0x3F => {
                // SWI — vector at $FFFA/$FFFB.
                // MC6809 spec push order: PC, U, Y, X, DP, B, A, CC
                // (deepest -> top). NMI/IRQ already do this. SWI was
                // historically reversed here, which placed CC at the
                // bottom and PC at the top of the saved frame, so the
                // matching RTI restored garbage into PC and other
                // registers. Aligning with NMI/IRQ also matches what
                // a real 6809 puts on the S stack, so guest code that
                // peeks at the saved frame (e.g., NetBSD trap glue)
                // sees what it expects.
                let ret = self.r.pc;
                self.r.cc |= E;
                self.push16(bus, self.r.pc);
                self.push16(bus, self.r.u);
                self.push16(bus, self.r.y);
                self.push16(bus, self.r.x);
                self.push8(bus, self.r.dp);
                self.push8(bus, self.r.b);
                self.push8(bus, self.r.a);
                self.push8(bus, self.r.cc);
                self.r.cc |= I | F;
                let hi = bus.read8(0xFFFA) as u16;
                let lo = bus.read8(0xFFFB) as u16;
                self.r.pc = (hi << 8) | lo;
                self.shadow_stack.push(CallFrame {
                    return_addr: ret,
                    call_site: pc0,
                    target: self.r.pc,
                    sp_at_call: self.r.s,
                    kind: CallKind::Swi(1),
                });
                consumed = 19;
                if trace {
                    println!("{:04X}: 3F        SWI", pc0);
                }
            }
            0x8D => {
                // BSR rel
                let off = self.fetch8(bus) as i8 as i16;
                let ret = self.r.pc;
                // Push return address on S
                self.r.s = self.r.s.wrapping_sub(1);
                bus.write8(self.r.s, (ret & 0x00FF) as u8);
                self.r.s = self.r.s.wrapping_sub(1);
                bus.write8(self.r.s, (ret >> 8) as u8);
                self.r.pc = (self.r.pc as i16).wrapping_add(off) as u16;
                self.shadow_stack.push(CallFrame {
                    return_addr: ret,
                    call_site: pc0,
                    target: self.r.pc,
                    sp_at_call: self.r.s,
                    kind: CallKind::Bsr,
                });
                consumed = 7;
                if trace {
                    println!("{:04X}: 8D {:02X}   BSR", pc0, (off as i8) as u8);
                }
            }
            0x34 => {
                // PSHS
                let pb = self.fetch8(bus);
                let mut bytes = 0u32;
                if (pb & 0x80) != 0 {
                    self.push16(bus, self.r.pc);
                    bytes += 2;
                }
                if (pb & 0x40) != 0 {
                    self.push16(bus, self.r.u);
                    bytes += 2;
                }
                if (pb & 0x20) != 0 {
                    self.push16(bus, self.r.y);
                    bytes += 2;
                }
                if (pb & 0x10) != 0 {
                    self.push16(bus, self.r.x);
                    bytes += 2;
                }
                if (pb & 0x08) != 0 {
                    self.push8(bus, self.r.dp);
                    bytes += 1;
                }
                if (pb & 0x04) != 0 {
                    self.push8(bus, self.r.b);
                    bytes += 1;
                }
                if (pb & 0x02) != 0 {
                    self.push8(bus, self.r.a);
                    bytes += 1;
                }
                if (pb & 0x01) != 0 {
                    self.push8(bus, self.r.cc);
                    bytes += 1;
                }
                consumed = 5 + bytes;
                if trace {
                    println!("{:04X}: 34 {:02X}   PSHS", pc0, pb);
                }
            }
            0x36 => {
                // PSHU
                let pb = self.fetch8(bus);
                let mut bytes = 0u32;
                if (pb & 0x80) != 0 {
                    self.upush16(bus, self.r.pc);
                    bytes += 2;
                }
                if (pb & 0x40) != 0 {
                    self.upush16(bus, self.r.s);
                    bytes += 2;
                }
                if (pb & 0x20) != 0 {
                    self.upush16(bus, self.r.y);
                    bytes += 2;
                }
                if (pb & 0x10) != 0 {
                    self.upush16(bus, self.r.x);
                    bytes += 2;
                }
                if (pb & 0x08) != 0 {
                    self.upush8(bus, self.r.dp);
                    bytes += 1;
                }
                if (pb & 0x04) != 0 {
                    self.upush8(bus, self.r.b);
                    bytes += 1;
                }
                if (pb & 0x02) != 0 {
                    self.upush8(bus, self.r.a);
                    bytes += 1;
                }
                if (pb & 0x01) != 0 {
                    self.upush8(bus, self.r.cc);
                    bytes += 1;
                }
                consumed = 5 + bytes;
                if trace {
                    println!("{:04X}: 36 {:02X}   PSHU", pc0, pb);
                }
            }
            0x35 => {
                // PULS
                let pb = self.fetch8(bus);
                let mut bytes = 0u32;
                if (pb & 0x01) != 0 {
                    self.r.cc = self.pull8(bus);
                    bytes += 1;
                }
                if (pb & 0x02) != 0 {
                    self.r.a = self.pull8(bus);
                    bytes += 1;
                }
                if (pb & 0x04) != 0 {
                    self.r.b = self.pull8(bus);
                    bytes += 1;
                }
                if (pb & 0x08) != 0 {
                    self.r.dp = self.pull8(bus);
                    bytes += 1;
                }
                if (pb & 0x10) != 0 {
                    self.r.x = self.pull16(bus);
                    bytes += 2;
                }
                if (pb & 0x20) != 0 {
                    self.r.y = self.pull16(bus);
                    bytes += 2;
                }
                if (pb & 0x40) != 0 {
                    self.r.u = self.pull16(bus);
                    bytes += 2;
                }
                if (pb & 0x80) != 0 {
                    self.r.pc = self.pull16(bus);
                    bytes += 2;
                    // `puls ...,pc` is a manual return: it pops the
                    // saved PC just like RTS does, so the matching
                    // BSR/JSR call frame should be retired here too.
                    // Without this, callees that return via
                    // `puls b,pc` (a common 6809 idiom) leave their
                    // CALL frame on the shadow stack indefinitely.
                    self.shadow_stack.pop_for_return(self.r.s);
                }
                consumed = 5 + bytes;
                if trace {
                    println!("{:04X}: 35 {:02X}   PULS", pc0, pb);
                }
            }
            0x37 => {
                // PULU
                let pb = self.fetch8(bus);
                let mut bytes = 0u32;
                if (pb & 0x01) != 0 {
                    self.r.cc = self.upull8(bus);
                    bytes += 1;
                }
                if (pb & 0x02) != 0 {
                    self.r.a = self.upull8(bus);
                    bytes += 1;
                }
                if (pb & 0x04) != 0 {
                    self.r.b = self.upull8(bus);
                    bytes += 1;
                }
                if (pb & 0x08) != 0 {
                    self.r.dp = self.upull8(bus);
                    bytes += 1;
                }
                if (pb & 0x10) != 0 {
                    self.r.x = self.upull16(bus);
                    bytes += 2;
                }
                if (pb & 0x20) != 0 {
                    self.r.y = self.upull16(bus);
                    bytes += 2;
                }
                if (pb & 0x40) != 0 {
                    self.r.s = self.upull16(bus);
                    bytes += 2;
                }
                if (pb & 0x80) != 0 {
                    self.r.pc = self.upull16(bus);
                    bytes += 2;
                }
                consumed = 5 + bytes;
                if trace {
                    println!("{:04X}: 37 {:02X}   PULU", pc0, pb);
                }
            }
            0xBD => {
                // JSR extended
                let addr = self.fetch16(bus);
                let ret = self.r.pc;
                self.r.s = self.r.s.wrapping_sub(1);
                bus.write8(self.r.s, (ret & 0x00FF) as u8);
                self.r.s = self.r.s.wrapping_sub(1);
                bus.write8(self.r.s, (ret >> 8) as u8);
                self.r.pc = addr;
                self.shadow_stack.push(CallFrame {
                    return_addr: ret,
                    call_site: pc0,
                    target: self.r.pc,
                    sp_at_call: self.r.s,
                    kind: CallKind::Jsr,
                });
                consumed = 7;
                if trace {
                    println!("{:04X}: BD {:04X} JSR ${:04X}", pc0, addr, addr);
                }
            }
            0x8E => {
                // LDX #imm16
                let imm = self.fetch16(bus);
                self.r.x = imm;
                self.set_nz16(self.r.x);
                self.r.cc &= !V;
                consumed = 3;
                if trace {
                    println!("{:04X}: 8E {:04X} LDX #${:04X}", pc0, imm, imm);
                }
            }
            // Accumulator shifts/rotates/bit ops (A)
            0x48 => {
                // ASLA
                let c_out = (self.r.a & 0x80) != 0;
                let res = self.r.a.wrapping_shl(1);
                self.r.a = res;
                self.set_nz8(res);
                let n = (res & 0x80) != 0;
                let c = c_out;
                self.r.cc = (self.r.cc & !(C | V))
                    | (if c { C } else { 0 })
                    | (if (n as u8) ^ (c as u8) != 0 { V } else { 0 });
                consumed = 2;
                if trace {
                    println!("{:04X}: 48        ASLA", pc0);
                }
            }
            0x44 => {
                // LSRA
                let c_out = (self.r.a & 0x01) != 0;
                let res = self.r.a >> 1;
                self.r.a = res;
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !(C | V | N)) | (if c_out { C } else { 0 }); // N cleared for LSR
                consumed = 2;
                if trace {
                    println!("{:04X}: 44        LSRA", pc0);
                }
            }
            0x49 => {
                // ROLA
                let c_in = (self.r.cc & C) != 0;
                let c_out = (self.r.a & 0x80) != 0;
                let res = (self.r.a << 1) | (if c_in { 1 } else { 0 });
                self.r.a = res;
                self.set_nz8(res);
                let n = (res & 0x80) != 0;
                let c = c_out;
                self.r.cc = (self.r.cc & !(C | V))
                    | (if c { C } else { 0 })
                    | (if (n as u8) ^ (c as u8) != 0 { V } else { 0 });
                consumed = 2;
                if trace {
                    println!("{:04X}: 49        ROLA", pc0);
                }
            }
            0x46 => {
                // RORA
                let c_in = (self.r.cc & C) != 0;
                let c_out = (self.r.a & 0x01) != 0;
                let res = (self.r.a >> 1) | (if c_in { 0x80 } else { 0 });
                self.r.a = res;
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !(C | V)) | (if c_out { C } else { 0 });
                consumed = 2;
                if trace {
                    println!("{:04X}: 46        RORA", pc0);
                }
            }
            0x43 => {
                // COMA
                let res = !self.r.a;
                self.r.a = res;
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !V) | C; // V=0, C=1
                consumed = 2;
                if trace {
                    println!("{:04X}: 43        COMA", pc0);
                }
            }
            0x4F => {
                // CLRA
                self.r.a = 0;
                self.set_nz8(0);
                self.r.cc &= !N;
                self.r.cc &= !V;
                self.r.cc &= !C;
                self.r.cc |= Z;
                consumed = 2;
                if trace {
                    println!("{:04X}: 4F        CLRA", pc0);
                }
            }
            0x4D => {
                // TSTA
                let v = self.r.a;
                self.set_nz8(v);
                self.r.cc &= !V;
                consumed = 2;
                if trace {
                    println!("{:04X}: 4D        TSTA", pc0);
                }
            }
            0x4A => {
                // DECA
                let prev = self.r.a;
                let res = prev.wrapping_sub(1);
                self.r.a = res;
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !V) | (if prev == 0x80 { V } else { 0 });
                consumed = 2;
                if trace {
                    println!("{:04X}: 4A        DECA", pc0);
                }
            }
            0x4C => {
                // INCA
                let prev = self.r.a;
                let res = prev.wrapping_add(1);
                self.r.a = res;
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !V) | (if prev == 0x7F { V } else { 0 });
                consumed = 2;
                if trace {
                    println!("{:04X}: 4C        INCA", pc0);
                }
            }
            0x40 => {
                // NEGA — A = -A.  C set iff A != 0, V set iff A was 0x80.
                let prev = self.r.a;
                let res = 0u8.wrapping_sub(prev);
                self.r.a = res;
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !(V | C))
                    | (if prev != 0 { C } else { 0 })
                    | (if prev == 0x80 { V } else { 0 });
                consumed = 2;
                if trace {
                    println!("{:04X}: 40        NEGA", pc0);
                }
            }
            0x47 => {
                // ASRA — arithmetic shift right (preserves sign bit).
                let c_out = (self.r.a & 0x01) != 0;
                let res = ((self.r.a as i8) >> 1) as u8;
                self.r.a = res;
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !C) | (if c_out { C } else { 0 });
                consumed = 2;
                if trace {
                    println!("{:04X}: 47        ASRA", pc0);
                }
            }
            0x50 => {
                // NEGB
                let prev = self.r.b;
                let res = 0u8.wrapping_sub(prev);
                self.r.b = res;
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !(V | C))
                    | (if prev != 0 { C } else { 0 })
                    | (if prev == 0x80 { V } else { 0 });
                consumed = 2;
                if trace {
                    println!("{:04X}: 50        NEGB", pc0);
                }
            }
            0x57 => {
                // ASRB
                let c_out = (self.r.b & 0x01) != 0;
                let res = ((self.r.b as i8) >> 1) as u8;
                self.r.b = res;
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !C) | (if c_out { C } else { 0 });
                consumed = 2;
                if trace {
                    println!("{:04X}: 57        ASRB", pc0);
                }
            }
            0x1D => {
                // SEX — sign-extend B into D (A = (B < 0) ? $FF : $00).
                self.r.a = if (self.r.b & 0x80) != 0 { 0xFF } else { 0 };
                let d = ((self.r.a as u16) << 8) | (self.r.b as u16);
                self.set_nz16(d);
                self.r.cc &= !V;
                consumed = 2;
                if trace {
                    println!("{:04X}: 1D        SEX", pc0);
                }
            }
            0x3D => {
                // MUL — D = A * B (unsigned 8x8 → 16).  Z from D, C = bit 7 of B.
                let d = (self.r.a as u16).wrapping_mul(self.r.b as u16);
                self.r.a = (d >> 8) as u8;
                self.r.b = (d & 0xFF) as u8;
                self.r.cc = (self.r.cc & !(Z | C))
                    | (if d == 0 { Z } else { 0 })
                    | (if (self.r.b & 0x80) != 0 { C } else { 0 });
                consumed = 11;
                if trace {
                    println!("{:04X}: 3D        MUL", pc0);
                }
            }
            0x19 => {
                // DAA — decimal adjust A after BCD add.  Per Motorola ref:
                //   If H==1 or (lo nibble > 9), add $06 to low nibble.
                //   If C==1 or (hi nibble > 9 after low fix), add $60 to high.
                let mut a = self.r.a as u16;
                let h = (self.r.cc & H) != 0;
                let c = (self.r.cc & C) != 0;
                let lo = a & 0x0F;
                let hi = (a >> 4) & 0x0F;
                let mut add: u16 = 0;
                if h || lo > 9 {
                    add |= 0x06;
                }
                if c || hi > 9 || (hi >= 9 && lo > 9) {
                    add |= 0x60;
                }
                a = a.wrapping_add(add);
                let carry_out = c || (a & 0x100) != 0;
                self.r.a = a as u8;
                self.set_nz8(self.r.a);
                self.r.cc = (self.r.cc & !C) | (if carry_out { C } else { 0 });
                consumed = 2;
                if trace {
                    println!("{:04X}: 19        DAA", pc0);
                }
            }
            // Accumulator shifts/rotates/bit ops (B)
            0x58 => {
                let c_out = (self.r.b & 0x80) != 0;
                let res = self.r.b.wrapping_shl(1);
                self.r.b = res;
                self.set_nz8(res);
                let n = (res & 0x80) != 0;
                let c = c_out;
                self.r.cc = (self.r.cc & !(C | V))
                    | (if c { C } else { 0 })
                    | (if (n as u8) ^ (c as u8) != 0 { V } else { 0 });
                consumed = 2;
                if trace {
                    println!("{:04X}: 58        ASLB", pc0);
                }
            }
            0x54 => {
                let c_out = (self.r.b & 0x01) != 0;
                let res = self.r.b >> 1;
                self.r.b = res;
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !(C | V | N)) | (if c_out { C } else { 0 });
                consumed = 2;
                if trace {
                    println!("{:04X}: 54        LSRB", pc0);
                }
            }
            0x59 => {
                let c_in = (self.r.cc & C) != 0;
                let c_out = (self.r.b & 0x80) != 0;
                let res = (self.r.b << 1) | (if c_in { 1 } else { 0 });
                self.r.b = res;
                self.set_nz8(res);
                let n = (res & 0x80) != 0;
                let c = c_out;
                self.r.cc = (self.r.cc & !(C | V))
                    | (if c { C } else { 0 })
                    | (if (n as u8) ^ (c as u8) != 0 { V } else { 0 });
                consumed = 2;
                if trace {
                    println!("{:04X}: 59        ROLB", pc0);
                }
            }
            0x56 => {
                let c_in = (self.r.cc & C) != 0;
                let c_out = (self.r.b & 0x01) != 0;
                let res = (self.r.b >> 1) | (if c_in { 0x80 } else { 0 });
                self.r.b = res;
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !(C | V)) | (if c_out { C } else { 0 });
                consumed = 2;
                if trace {
                    println!("{:04X}: 56        RORB", pc0);
                }
            }
            0x53 => {
                let res = !self.r.b;
                self.r.b = res;
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !V) | C;
                consumed = 2;
                if trace {
                    println!("{:04X}: 53        COMB", pc0);
                }
            }
            0x5F => {
                self.r.b = 0;
                self.set_nz8(0);
                self.r.cc &= !N;
                self.r.cc &= !V;
                self.r.cc &= !C;
                self.r.cc |= Z;
                consumed = 2;
                if trace {
                    println!("{:04X}: 5F        CLRB", pc0);
                }
            }
            0x5D => {
                let v = self.r.b;
                self.set_nz8(v);
                self.r.cc &= !V;
                consumed = 2;
                if trace {
                    println!("{:04X}: 5D        TSTB", pc0);
                }
            }
            0x5A => {
                let prev = self.r.b;
                let res = prev.wrapping_sub(1);
                self.r.b = res;
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !V) | (if prev == 0x80 { V } else { 0 });
                consumed = 2;
                if trace {
                    println!("{:04X}: 5A        DECB", pc0);
                }
            }
            0x5C => {
                let prev = self.r.b;
                let res = prev.wrapping_add(1);
                self.r.b = res;
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !V) | (if prev == 0x7F { V } else { 0 });
                consumed = 2;
                if trace {
                    println!("{:04X}: 5C        INCB", pc0);
                }
            }

            // Memory NEG variants.  res = -v; C = (v != 0); V = (v == 0x80).
            0x00 => {
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                let v = bus.read8(addr);
                let res = 0u8.wrapping_sub(v);
                bus.write8(addr, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !(V | C))
                    | (if v != 0 { C } else { 0 })
                    | (if v == 0x80 { V } else { 0 });
                consumed = 6;
                if trace {
                    println!("{:04X}: 00 {:02X}    NEG <${:02X}", pc0, zp, zp);
                }
            }
            0x60 => {
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let res = 0u8.wrapping_sub(v);
                bus.write8(ea, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !(V | C))
                    | (if v != 0 { C } else { 0 })
                    | (if v == 0x80 { V } else { 0 });
                consumed = 6 + addcyc;
                if trace {
                    println!("{:04X}: 60 ..    NEG {}", pc0, desc);
                }
            }
            0x70 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let res = 0u8.wrapping_sub(v);
                bus.write8(addr, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !(V | C))
                    | (if v != 0 { C } else { 0 })
                    | (if v == 0x80 { V } else { 0 });
                consumed = 7;
                if trace {
                    println!("{:04X}: 70 {:04X} NEG ${:04X}", pc0, addr, addr);
                }
            }

            // Memory COM variants.  res = !v; V=0; C=1.
            0x03 => {
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                let res = !bus.read8(addr);
                bus.write8(addr, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !V) | C;
                consumed = 6;
                if trace {
                    println!("{:04X}: 03 {:02X}    COM <${:02X}", pc0, zp, zp);
                }
            }
            0x63 => {
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                let res = !bus.read8(ea);
                bus.write8(ea, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !V) | C;
                consumed = 6 + addcyc;
                if trace {
                    println!("{:04X}: 63 ..    COM {}", pc0, desc);
                }
            }
            0x73 => {
                let addr = self.ea_extended(bus);
                let res = !bus.read8(addr);
                bus.write8(addr, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !V) | C;
                consumed = 7;
                if trace {
                    println!("{:04X}: 73 {:04X} COM ${:04X}", pc0, addr, addr);
                }
            }

            // Memory LSR variants.  res = v >> 1; C = v bit0; N cleared.
            0x04 => {
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                let v = bus.read8(addr);
                let c_out = (v & 0x01) != 0;
                let res = v >> 1;
                bus.write8(addr, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !(C | V | N)) | (if c_out { C } else { 0 });
                consumed = 6;
                if trace {
                    println!("{:04X}: 04 {:02X}    LSR <${:02X}", pc0, zp, zp);
                }
            }
            0x64 => {
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let c_out = (v & 0x01) != 0;
                let res = v >> 1;
                bus.write8(ea, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !(C | V | N)) | (if c_out { C } else { 0 });
                consumed = 6 + addcyc;
                if trace {
                    println!("{:04X}: 64 ..    LSR {}", pc0, desc);
                }
            }
            0x74 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let c_out = (v & 0x01) != 0;
                let res = v >> 1;
                bus.write8(addr, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !(C | V | N)) | (if c_out { C } else { 0 });
                consumed = 7;
                if trace {
                    println!("{:04X}: 74 {:04X} LSR ${:04X}", pc0, addr, addr);
                }
            }

            // Memory ROR variants.  res = (v >> 1) | (C_in << 7); C_out = v bit0.
            0x06 => {
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                let v = bus.read8(addr);
                let c_in = (self.r.cc & C) != 0;
                let c_out = (v & 0x01) != 0;
                let res = (v >> 1) | (if c_in { 0x80 } else { 0 });
                bus.write8(addr, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !(C | V)) | (if c_out { C } else { 0 });
                consumed = 6;
                if trace {
                    println!("{:04X}: 06 {:02X}    ROR <${:02X}", pc0, zp, zp);
                }
            }
            0x66 => {
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let c_in = (self.r.cc & C) != 0;
                let c_out = (v & 0x01) != 0;
                let res = (v >> 1) | (if c_in { 0x80 } else { 0 });
                bus.write8(ea, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !(C | V)) | (if c_out { C } else { 0 });
                consumed = 6 + addcyc;
                if trace {
                    println!("{:04X}: 66 ..    ROR {}", pc0, desc);
                }
            }
            0x76 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let c_in = (self.r.cc & C) != 0;
                let c_out = (v & 0x01) != 0;
                let res = (v >> 1) | (if c_in { 0x80 } else { 0 });
                bus.write8(addr, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !(C | V)) | (if c_out { C } else { 0 });
                consumed = 7;
                if trace {
                    println!("{:04X}: 76 {:04X} ROR ${:04X}", pc0, addr, addr);
                }
            }

            // Memory ASR variants.  Arithmetic shift right (preserves sign bit).
            0x07 => {
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                let v = bus.read8(addr);
                let c_out = (v & 0x01) != 0;
                let res = ((v as i8) >> 1) as u8;
                bus.write8(addr, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !C) | (if c_out { C } else { 0 });
                consumed = 6;
                if trace {
                    println!("{:04X}: 07 {:02X}    ASR <${:02X}", pc0, zp, zp);
                }
            }
            0x67 => {
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let c_out = (v & 0x01) != 0;
                let res = ((v as i8) >> 1) as u8;
                bus.write8(ea, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !C) | (if c_out { C } else { 0 });
                consumed = 6 + addcyc;
                if trace {
                    println!("{:04X}: 67 ..    ASR {}", pc0, desc);
                }
            }
            0x77 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let c_out = (v & 0x01) != 0;
                let res = ((v as i8) >> 1) as u8;
                bus.write8(addr, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !C) | (if c_out { C } else { 0 });
                consumed = 7;
                if trace {
                    println!("{:04X}: 77 {:04X} ASR ${:04X}", pc0, addr, addr);
                }
            }

            // Memory ASL/LSL variants.  res = v << 1; C = v bit7; V = N xor C.
            0x08 => {
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                let v = bus.read8(addr);
                let c_out = (v & 0x80) != 0;
                let res = v.wrapping_shl(1);
                bus.write8(addr, res);
                self.set_nz8(res);
                let n = (res & 0x80) != 0;
                self.r.cc = (self.r.cc & !(C | V))
                    | (if c_out { C } else { 0 })
                    | (if (n as u8) ^ (c_out as u8) != 0 { V } else { 0 });
                consumed = 6;
                if trace {
                    println!("{:04X}: 08 {:02X}    ASL <${:02X}", pc0, zp, zp);
                }
            }
            0x68 => {
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let c_out = (v & 0x80) != 0;
                let res = v.wrapping_shl(1);
                bus.write8(ea, res);
                self.set_nz8(res);
                let n = (res & 0x80) != 0;
                self.r.cc = (self.r.cc & !(C | V))
                    | (if c_out { C } else { 0 })
                    | (if (n as u8) ^ (c_out as u8) != 0 { V } else { 0 });
                consumed = 6 + addcyc;
                if trace {
                    println!("{:04X}: 68 ..    ASL {}", pc0, desc);
                }
            }
            0x78 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let c_out = (v & 0x80) != 0;
                let res = v.wrapping_shl(1);
                bus.write8(addr, res);
                self.set_nz8(res);
                let n = (res & 0x80) != 0;
                self.r.cc = (self.r.cc & !(C | V))
                    | (if c_out { C } else { 0 })
                    | (if (n as u8) ^ (c_out as u8) != 0 { V } else { 0 });
                consumed = 7;
                if trace {
                    println!("{:04X}: 78 {:04X} ASL ${:04X}", pc0, addr, addr);
                }
            }

            // Memory ROL variants.  res = (v << 1) | C_in; C_out = v bit7; V = N xor C.
            0x09 => {
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                let v = bus.read8(addr);
                let c_in = (self.r.cc & C) != 0;
                let c_out = (v & 0x80) != 0;
                let res = (v << 1) | (if c_in { 1 } else { 0 });
                bus.write8(addr, res);
                self.set_nz8(res);
                let n = (res & 0x80) != 0;
                self.r.cc = (self.r.cc & !(C | V))
                    | (if c_out { C } else { 0 })
                    | (if (n as u8) ^ (c_out as u8) != 0 { V } else { 0 });
                consumed = 6;
                if trace {
                    println!("{:04X}: 09 {:02X}    ROL <${:02X}", pc0, zp, zp);
                }
            }
            0x69 => {
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let c_in = (self.r.cc & C) != 0;
                let c_out = (v & 0x80) != 0;
                let res = (v << 1) | (if c_in { 1 } else { 0 });
                bus.write8(ea, res);
                self.set_nz8(res);
                let n = (res & 0x80) != 0;
                self.r.cc = (self.r.cc & !(C | V))
                    | (if c_out { C } else { 0 })
                    | (if (n as u8) ^ (c_out as u8) != 0 { V } else { 0 });
                consumed = 6 + addcyc;
                if trace {
                    println!("{:04X}: 69 ..    ROL {}", pc0, desc);
                }
            }
            0x79 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let c_in = (self.r.cc & C) != 0;
                let c_out = (v & 0x80) != 0;
                let res = (v << 1) | (if c_in { 1 } else { 0 });
                bus.write8(addr, res);
                self.set_nz8(res);
                let n = (res & 0x80) != 0;
                self.r.cc = (self.r.cc & !(C | V))
                    | (if c_out { C } else { 0 })
                    | (if (n as u8) ^ (c_out as u8) != 0 { V } else { 0 });
                consumed = 7;
                if trace {
                    println!("{:04X}: 79 {:04X} ROL ${:04X}", pc0, addr, addr);
                }
            }

            // Memory DEC variants.  res = v - 1; V = (v == 0x80).
            0x0A => {
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                let v = bus.read8(addr);
                let res = v.wrapping_sub(1);
                bus.write8(addr, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !V) | (if v == 0x80 { V } else { 0 });
                consumed = 6;
                if trace {
                    println!("{:04X}: 0A {:02X}    DEC <${:02X}", pc0, zp, zp);
                }
            }
            0x6A => {
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let res = v.wrapping_sub(1);
                bus.write8(ea, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !V) | (if v == 0x80 { V } else { 0 });
                consumed = 6 + addcyc;
                if trace {
                    println!("{:04X}: 6A ..    DEC {}", pc0, desc);
                }
            }
            0x7A => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let res = v.wrapping_sub(1);
                bus.write8(addr, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !V) | (if v == 0x80 { V } else { 0 });
                consumed = 7;
                if trace {
                    println!("{:04X}: 7A {:04X} DEC ${:04X}", pc0, addr, addr);
                }
            }

            // Memory TST variants — set N/Z from the byte at EA, clear V.
            0x0D => {
                // TST <dir
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                let v = bus.read8(addr);
                self.set_nz8(v);
                self.r.cc &= !V;
                consumed = 6;
                if trace {
                    println!("{:04X}: 0D {:02X}    TST <${:02X}", pc0, zp, zp);
                }
            }
            0x6D => {
                // TST indexed
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                self.set_nz8(v);
                self.r.cc &= !V;
                consumed = 6 + addcyc;
                if trace {
                    println!("{:04X}: 6D ..    TST {}", pc0, desc);
                }
            }
            0x7D => {
                // TST extended
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                self.set_nz8(v);
                self.r.cc &= !V;
                consumed = 7;
                if trace {
                    println!("{:04X}: 7D {:04X} TST ${:04X}", pc0, addr, addr);
                }
            }

            // Memory INC variants — increment the byte at EA; set N/Z; V set iff prev==0x7F.
            0x0C => {
                // INC <dir
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                let prev = bus.read8(addr);
                let res = prev.wrapping_add(1);
                bus.write8(addr, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !V) | (if prev == 0x7F { V } else { 0 });
                consumed = 6;
                if trace {
                    println!("{:04X}: 0C {:02X}    INC <${:02X}", pc0, zp, zp);
                }
            }
            0x6C => {
                // INC indexed
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                let prev = bus.read8(ea);
                let res = prev.wrapping_add(1);
                bus.write8(ea, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !V) | (if prev == 0x7F { V } else { 0 });
                consumed = 6 + addcyc;
                if trace {
                    println!("{:04X}: 6C ..    INC {}", pc0, desc);
                }
            }
            0x7C => {
                // INC extended
                let addr = self.ea_extended(bus);
                let prev = bus.read8(addr);
                let res = prev.wrapping_add(1);
                bus.write8(addr, res);
                self.set_nz8(res);
                self.r.cc = (self.r.cc & !V) | (if prev == 0x7F { V } else { 0 });
                consumed = 7;
                if trace {
                    println!("{:04X}: 7C {:04X} INC ${:04X}", pc0, addr, addr);
                }
            }

            // Memory clear (CLR) variants
            0x0F => {
                // CLR <dir
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                bus.write8(addr, 0);
                self.set_nz8(0);
                self.r.cc &= !N;
                self.r.cc &= !V;
                self.r.cc &= !C;
                self.r.cc |= Z;
                consumed = 6;
                if trace {
                    println!("{:04X}: 0F {:02X}    CLR <${:02X}", pc0, zp, zp);
                }
            }
            0x6F => {
                // CLR indexed
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                bus.write8(ea, 0);
                self.set_nz8(0);
                self.r.cc &= !N;
                self.r.cc &= !V;
                self.r.cc &= !C;
                self.r.cc |= Z;
                consumed = 6 + addcyc;
                if trace {
                    println!("{:04X}: 6F ..    CLR {}", pc0, desc);
                }
            }
            0x7F => {
                // CLR extended
                let addr = self.ea_extended(bus);
                bus.write8(addr, 0);
                self.set_nz8(0);
                self.r.cc &= !N;
                self.r.cc &= !V;
                self.r.cc &= !C;
                self.r.cc |= Z;
                consumed = 7;
                if trace {
                    println!("{:04X}: 7F {:04X} CLR ${:04X}", pc0, addr, addr);
                }
            }

            // 16-bit arithmetic on D (A:B)
            0x83 => {
                // SUBD #imm16
                let imm = self.fetch16(bus);
                let d = ((self.r.a as u16) << 8) | (self.r.b as u16);
                let res = self.sub16_flags(d, imm);
                self.r.a = (res >> 8) as u8;
                self.r.b = (res & 0xFF) as u8;
                consumed = 4;
                if trace {
                    println!("{:04X}: 83 {:04X} SUBD #${:04X}", pc0, imm, imm);
                }
            }
            0x93 => {
                // SUBD direct
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                let hi = bus.read8(addr) as u16;
                let lo = bus.read8(addr.wrapping_add(1)) as u16;
                let imm = (hi << 8) | lo;
                let d = ((self.r.a as u16) << 8) | (self.r.b as u16);
                let res = self.sub16_flags(d, imm);
                self.r.a = (res >> 8) as u8;
                self.r.b = (res & 0xFF) as u8;
                consumed = 6;
                if trace {
                    println!("{:04X}: 93 {:02X}   SUBD ${:02X}", pc0, zp, zp);
                }
            }
            0xA3 => {
                // SUBD indexed
                let (ea, add, desc) = self.ea_indexed(bus);
                let hi = bus.read8(ea) as u16;
                let lo = bus.read8(ea.wrapping_add(1)) as u16;
                let imm = (hi << 8) | lo;
                let d = ((self.r.a as u16) << 8) | (self.r.b as u16);
                let res = self.sub16_flags(d, imm);
                self.r.a = (res >> 8) as u8;
                self.r.b = (res & 0xFF) as u8;
                consumed = 6 + add;
                if trace {
                    println!("{:04X}: A3 ..    SUBD {}", pc0, desc);
                }
            }
            0xB3 => {
                // SUBD extended
                let addr = self.ea_extended(bus);
                let hi = bus.read8(addr) as u16;
                let lo = bus.read8(addr.wrapping_add(1)) as u16;
                let imm = (hi << 8) | lo;
                let d = ((self.r.a as u16) << 8) | (self.r.b as u16);
                let res = self.sub16_flags(d, imm);
                self.r.a = (res >> 8) as u8;
                self.r.b = (res & 0xFF) as u8;
                consumed = 7;
                if trace {
                    println!("{:04X}: B3 {:04X} SUBD ${:04X}", pc0, addr, addr);
                }
            }
            0xC3 => {
                // ADDD #imm16
                let imm = self.fetch16(bus);
                let d = ((self.r.a as u16) << 8) | (self.r.b as u16);
                let res = self.add16_flags(d, imm);
                self.r.a = (res >> 8) as u8;
                self.r.b = (res & 0xFF) as u8;
                consumed = 4;
                if trace {
                    println!("{:04X}: C3 {:04X} ADDD #${:04X}", pc0, imm, imm);
                }
            }
            0xD3 => {
                // ADDD direct
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                let hi = bus.read8(addr) as u16;
                let lo = bus.read8(addr.wrapping_add(1)) as u16;
                let imm = (hi << 8) | lo;
                let d = ((self.r.a as u16) << 8) | (self.r.b as u16);
                let res = self.add16_flags(d, imm);
                self.r.a = (res >> 8) as u8;
                self.r.b = (res & 0xFF) as u8;
                consumed = 6;
                if trace {
                    println!("{:04X}: D3 {:02X}   ADDD ${:02X}", pc0, zp, zp);
                }
            }
            0xE3 => {
                // ADDD indexed
                let (ea, add, desc) = self.ea_indexed(bus);
                let hi = bus.read8(ea) as u16;
                let lo = bus.read8(ea.wrapping_add(1)) as u16;
                let imm = (hi << 8) | lo;
                let d = ((self.r.a as u16) << 8) | (self.r.b as u16);
                let res = self.add16_flags(d, imm);
                self.r.a = (res >> 8) as u8;
                self.r.b = (res & 0xFF) as u8;
                consumed = 6 + add;
                if trace {
                    println!("{:04X}: E3 ..    ADDD {}", pc0, desc);
                }
            }
            0xF3 => {
                // ADDD extended
                let addr = self.ea_extended(bus);
                let hi = bus.read8(addr) as u16;
                let lo = bus.read8(addr.wrapping_add(1)) as u16;
                let imm = (hi << 8) | lo;
                let d = ((self.r.a as u16) << 8) | (self.r.b as u16);
                let res = self.add16_flags(d, imm);
                self.r.a = (res >> 8) as u8;
                self.r.b = (res & 0xFF) as u8;
                consumed = 7;
                if trace {
                    println!("{:04X}: F3 {:04X} ADDD ${:04X}", pc0, addr, addr);
                }
            }
            // 8-bit ALU A immediate group
            0x80 => {
                // SUBA #
                let imm = self.fetch8(bus);
                let a = self.r.a;
                self.r.a = self.sub8_flags(a, imm);
                consumed = 2;
                if trace {
                    println!("{:04X}: 80 {:02X}   SUBA #${:02X}", pc0, imm, imm);
                }
            }
            0x81 => {
                // CMPA #
                let imm = self.fetch8(bus);
                let a = self.r.a;
                let _ = self.sub8_flags(a, imm);
                self.r.a = a; // no store
                consumed = 2;
                if trace {
                    println!("{:04X}: 81 {:02X}   CMPA #${:02X}", pc0, imm, imm);
                }
            }
            0x82 => {
                // SBCA #
                let imm = self.fetch8(bus);
                let a = self.r.a;
                self.r.a = self.sbc8_flags(a, imm);
                consumed = 2;
                if trace {
                    println!("{:04X}: 82 {:02X}   SBCA #${:02X}", pc0, imm, imm);
                }
            }
            0x84 => {
                // ANDA #
                let imm = self.fetch8(bus);
                let v = self.r.a & imm;
                self.logic8_nz_clearv(v);
                self.r.a = v;
                consumed = 2;
                if trace {
                    println!("{:04X}: 84 {:02X}   ANDA #${:02X}", pc0, imm, imm);
                }
            }
            0x85 => {
                // BITA # (test)
                let imm = self.fetch8(bus);
                let v = self.r.a & imm;
                self.logic8_nz_clearv(v);
                consumed = 2;
                if trace {
                    println!("{:04X}: 85 {:02X}   BITA #${:02X}", pc0, imm, imm);
                }
            }
            0x88 => {
                // EORA #
                let imm = self.fetch8(bus);
                let v = self.r.a ^ imm;
                self.logic8_nz_clearv(v);
                self.r.a = v;
                consumed = 2;
                if trace {
                    println!("{:04X}: 88 {:02X}   EORA #${:02X}", pc0, imm, imm);
                }
            }
            0x89 => {
                // ADCA #
                let imm = self.fetch8(bus);
                let a = self.r.a;
                self.r.a = self.adc8_flags(a, imm);
                consumed = 2;
                if trace {
                    println!("{:04X}: 89 {:02X}   ADCA #${:02X}", pc0, imm, imm);
                }
            }
            0x8A => {
                // ORA #
                let imm = self.fetch8(bus);
                let v = self.r.a | imm;
                self.logic8_nz_clearv(v);
                self.r.a = v;
                consumed = 2;
                if trace {
                    println!("{:04X}: 8A {:02X}   ORA #${:02X}", pc0, imm, imm);
                }
            }
            // Direct variants A (0x90..)
            0x90 => {
                let zp = self.fetch8(bus);
                let v = bus.read8(self.ea_direct(zp));
                self.r.a = self.sub8_flags(self.r.a, v);
                consumed = 4;
                if trace {
                    println!("{:04X}: 90 {:02X}   SUBA ${:02X}", pc0, zp, zp);
                }
            }
            0x91 => {
                let zp = self.fetch8(bus);
                let v = bus.read8(self.ea_direct(zp));
                let a = self.r.a;
                let _ = self.sub8_flags(a, v);
                self.r.a = a;
                consumed = 4;
                if trace {
                    println!("{:04X}: 91 {:02X}   CMPA ${:02X}", pc0, zp, zp);
                }
            }
            0x92 => {
                let zp = self.fetch8(bus);
                let v = bus.read8(self.ea_direct(zp));
                let a = self.r.a;
                self.r.a = self.sbc8_flags(a, v);
                consumed = 4;
                if trace {
                    println!("{:04X}: 92 {:02X}   SBCA ${:02X}", pc0, zp, zp);
                }
            }
            0x94 => {
                let zp = self.fetch8(bus);
                let v = bus.read8(self.ea_direct(zp));
                let res = self.r.a & v;
                self.logic8_nz_clearv(res);
                self.r.a = res;
                consumed = 4;
                if trace {
                    println!("{:04X}: 94 {:02X}   ANDA ${:02X}", pc0, zp, zp);
                }
            }
            0x95 => {
                let zp = self.fetch8(bus);
                let v = bus.read8(self.ea_direct(zp));
                let res = self.r.a & v;
                self.logic8_nz_clearv(res);
                consumed = 4;
                if trace {
                    println!("{:04X}: 95 {:02X}   BITA ${:02X}", pc0, zp, zp);
                }
            }
            0x98 => {
                let zp = self.fetch8(bus);
                let v = bus.read8(self.ea_direct(zp));
                let res = self.r.a ^ v;
                self.logic8_nz_clearv(res);
                self.r.a = res;
                consumed = 4;
                if trace {
                    println!("{:04X}: 98 {:02X}   EORA ${:02X}", pc0, zp, zp);
                }
            }
            0x99 => {
                let zp = self.fetch8(bus);
                let v = bus.read8(self.ea_direct(zp));
                let a = self.r.a;
                self.r.a = self.adc8_flags(a, v);
                consumed = 4;
                if trace {
                    println!("{:04X}: 99 {:02X}   ADCA ${:02X}", pc0, zp, zp);
                }
            }
            0x9A => {
                let zp = self.fetch8(bus);
                let v = bus.read8(self.ea_direct(zp));
                let res = self.r.a | v;
                self.logic8_nz_clearv(res);
                self.r.a = res;
                consumed = 4;
                if trace {
                    println!("{:04X}: 9A {:02X}   ORA ${:02X}", pc0, zp, zp);
                }
            }
            0x9B => {
                let zp = self.fetch8(bus);
                let v = bus.read8(self.ea_direct(zp));
                let a = self.r.a;
                self.r.a = self.add8_flags(a, v);
                consumed = 4;
                if trace {
                    println!("{:04X}: 9B {:02X}   ADDA ${:02X}", pc0, zp, zp);
                }
            }
            // Indexed variants A (0xA0..)
            0xA0 => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                self.r.a = self.sub8_flags(self.r.a, v);
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: A0 ..    SUBA {}", pc0, desc);
                }
            }
            0xA1 => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let a = self.r.a;
                let _ = self.sub8_flags(a, v);
                self.r.a = a;
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: A1 ..    CMPA {}", pc0, desc);
                }
            }
            0xA2 => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let a = self.r.a;
                self.r.a = self.sbc8_flags(a, v);
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: A2 ..    SBCA {}", pc0, desc);
                }
            }
            0xA4 => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let res = self.r.a & v;
                self.logic8_nz_clearv(res);
                self.r.a = res;
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: A4 ..    ANDA {}", pc0, desc);
                }
            }
            0xA5 => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let res = self.r.a & v;
                self.logic8_nz_clearv(res);
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: A5 ..    BITA {}", pc0, desc);
                }
            }
            0xA8 => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let res = self.r.a ^ v;
                self.logic8_nz_clearv(res);
                self.r.a = res;
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: A8 ..    EORA {}", pc0, desc);
                }
            }
            0xA9 => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let a = self.r.a;
                self.r.a = self.adc8_flags(a, v);
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: A9 ..    ADCA {}", pc0, desc);
                }
            }
            0xAA => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let res = self.r.a | v;
                self.logic8_nz_clearv(res);
                self.r.a = res;
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: AA ..    ORA {}", pc0, desc);
                }
            }
            0xAB => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let a = self.r.a;
                self.r.a = self.add8_flags(a, v);
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: AB ..    ADDA {}", pc0, desc);
                }
            }
            // Extended variants A (0xB0..)
            0xB0 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                self.r.a = self.sub8_flags(self.r.a, v);
                consumed = 5;
                if trace {
                    println!("{:04X}: B0 {:04X} SUBA ${:04X}", pc0, addr, addr);
                }
            }
            0xB1 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let a = self.r.a;
                let _ = self.sub8_flags(a, v);
                self.r.a = a;
                consumed = 5;
                if trace {
                    println!("{:04X}: B1 {:04X} CMPA ${:04X}", pc0, addr, addr);
                }
            }
            0xB2 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let a = self.r.a;
                self.r.a = self.sbc8_flags(a, v);
                consumed = 5;
                if trace {
                    println!("{:04X}: B2 {:04X} SBCA ${:04X}", pc0, addr, addr);
                }
            }
            0xB4 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let res = self.r.a & v;
                self.logic8_nz_clearv(res);
                self.r.a = res;
                consumed = 5;
                if trace {
                    println!("{:04X}: B4 {:04X} ANDA ${:04X}", pc0, addr, addr);
                }
            }
            0xB5 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let res = self.r.a & v;
                self.logic8_nz_clearv(res);
                consumed = 5;
                if trace {
                    println!("{:04X}: B5 {:04X} BITA ${:04X}", pc0, addr, addr);
                }
            }
            0xB8 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let res = self.r.a ^ v;
                self.logic8_nz_clearv(res);
                self.r.a = res;
                consumed = 5;
                if trace {
                    println!("{:04X}: B8 {:04X} EORA ${:04X}", pc0, addr, addr);
                }
            }
            0xB9 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let a = self.r.a;
                self.r.a = self.adc8_flags(a, v);
                consumed = 5;
                if trace {
                    println!("{:04X}: B9 {:04X} ADCA ${:04X}", pc0, addr, addr);
                }
            }
            0xBA => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let res = self.r.a | v;
                self.logic8_nz_clearv(res);
                self.r.a = res;
                consumed = 5;
                if trace {
                    println!("{:04X}: BA {:04X} ORA ${:04X}", pc0, addr, addr);
                }
            }
            0xBB => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let a = self.r.a;
                self.r.a = self.add8_flags(a, v);
                consumed = 5;
                if trace {
                    println!("{:04X}: BB {:04X} ADDA ${:04X}", pc0, addr, addr);
                }
            }

            // 8-bit ALU B immediate group (0xC0..)
            0xC0 => {
                let imm = self.fetch8(bus);
                let b = self.r.b;
                self.r.b = self.sub8_flags(b, imm);
                consumed = 2;
                if trace {
                    println!("{:04X}: C0 {:02X}   SUBB #${:02X}", pc0, imm, imm);
                }
            }
            0xC1 => {
                let imm = self.fetch8(bus);
                let b = self.r.b;
                let _ = self.sub8_flags(b, imm);
                self.r.b = b;
                consumed = 2;
                if trace {
                    println!("{:04X}: C1 {:02X}   CMPB #${:02X}", pc0, imm, imm);
                }
            }
            0xC2 => {
                let imm = self.fetch8(bus);
                let b = self.r.b;
                self.r.b = self.sbc8_flags(b, imm);
                consumed = 2;
                if trace {
                    println!("{:04X}: C2 {:02X}   SBCB #${:02X}", pc0, imm, imm);
                }
            }
            0xC4 => {
                let imm = self.fetch8(bus);
                let v = self.r.b & imm;
                self.logic8_nz_clearv(v);
                self.r.b = v;
                consumed = 2;
                if trace {
                    println!("{:04X}: C4 {:02X}   ANDB #${:02X}", pc0, imm, imm);
                }
            }
            0xC5 => {
                let imm = self.fetch8(bus);
                let v = self.r.b & imm;
                self.logic8_nz_clearv(v);
                consumed = 2;
                if trace {
                    println!("{:04X}: C5 {:02X}   BITB #${:02X}", pc0, imm, imm);
                }
            }
            0xC6 => {
                let imm = self.fetch8(bus);
                self.r.b = imm;
                self.set_nz8(imm);
                self.r.cc &= !V;
                consumed = 2;
                if trace {
                    println!("{:04X}: C6 {:02X}   LDB #${:02X}", pc0, imm, imm);
                }
            }
            0xC8 => {
                let imm = self.fetch8(bus);
                let v = self.r.b ^ imm;
                self.logic8_nz_clearv(v);
                self.r.b = v;
                consumed = 2;
                if trace {
                    println!("{:04X}: C8 {:02X}   EORB #${:02X}", pc0, imm, imm);
                }
            }
            0xC9 => {
                let imm = self.fetch8(bus);
                let b = self.r.b;
                self.r.b = self.adc8_flags(b, imm);
                consumed = 2;
                if trace {
                    println!("{:04X}: C9 {:02X}   ADCB #${:02X}", pc0, imm, imm);
                }
            }
            0xCA => {
                let imm = self.fetch8(bus);
                let v = self.r.b | imm;
                self.logic8_nz_clearv(v);
                self.r.b = v;
                consumed = 2;
                if trace {
                    println!("{:04X}: CA {:02X}   ORB #${:02X}", pc0, imm, imm);
                }
            }
            0xCB => {
                let imm = self.fetch8(bus);
                let b = self.r.b;
                self.r.b = self.add8_flags(b, imm);
                consumed = 2;
                if trace {
                    println!("{:04X}: CB {:02X}   ADDB #${:02X}", pc0, imm, imm);
                }
            }
            // Direct B (0xD0..)
            0xD0 => {
                let zp = self.fetch8(bus);
                let v = bus.read8(self.ea_direct(zp));
                self.r.b = self.sub8_flags(self.r.b, v);
                consumed = 4;
                if trace {
                    println!("{:04X}: D0 {:02X}   SUBB ${:02X}", pc0, zp, zp);
                }
            }
            0xD1 => {
                let zp = self.fetch8(bus);
                let v = bus.read8(self.ea_direct(zp));
                let b = self.r.b;
                let _ = self.sub8_flags(b, v);
                self.r.b = b;
                consumed = 4;
                if trace {
                    println!("{:04X}: D1 {:02X}   CMPB ${:02X}", pc0, zp, zp);
                }
            }
            0xD2 => {
                let zp = self.fetch8(bus);
                let v = bus.read8(self.ea_direct(zp));
                let b = self.r.b;
                self.r.b = self.sbc8_flags(b, v);
                consumed = 4;
                if trace {
                    println!("{:04X}: D2 {:02X}   SBCB ${:02X}", pc0, zp, zp);
                }
            }
            0xD4 => {
                let zp = self.fetch8(bus);
                let v = bus.read8(self.ea_direct(zp));
                let res = self.r.b & v;
                self.logic8_nz_clearv(res);
                self.r.b = res;
                consumed = 4;
                if trace {
                    println!("{:04X}: D4 {:02X}   ANDB ${:02X}", pc0, zp, zp);
                }
            }
            0xD5 => {
                let zp = self.fetch8(bus);
                let v = bus.read8(self.ea_direct(zp));
                let res = self.r.b & v;
                self.logic8_nz_clearv(res);
                consumed = 4;
                if trace {
                    println!("{:04X}: D5 {:02X}   BITB ${:02X}", pc0, zp, zp);
                }
            }
            0xD6 => {
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                let v = bus.read8(addr);
                self.r.b = v;
                self.set_nz8(v);
                self.r.cc &= !V;
                consumed = 4;
                if trace {
                    println!("{:04X}: D6 {:02X}   LDB ${:02X}", pc0, zp, zp);
                }
            }
            0xD7 => {
                let zp = self.fetch8(bus);
                let addr = self.ea_direct(zp);
                bus.write8(addr, self.r.b);
                self.set_nz8(self.r.b);
                self.r.cc &= !V;
                consumed = 4;
                if trace {
                    println!("{:04X}: D7 {:02X}   STB ${:02X}", pc0, zp, zp);
                }
            }
            0xD8 => {
                let zp = self.fetch8(bus);
                let v = bus.read8(self.ea_direct(zp));
                let res = self.r.b ^ v;
                self.logic8_nz_clearv(res);
                self.r.b = res;
                consumed = 4;
                if trace {
                    println!("{:04X}: D8 {:02X}   EORB ${:02X}", pc0, zp, zp);
                }
            }
            0xD9 => {
                let zp = self.fetch8(bus);
                let v = bus.read8(self.ea_direct(zp));
                let b = self.r.b;
                self.r.b = self.adc8_flags(b, v);
                consumed = 4;
                if trace {
                    println!("{:04X}: D9 {:02X}   ADCB ${:02X}", pc0, zp, zp);
                }
            }
            0xDA => {
                let zp = self.fetch8(bus);
                let v = bus.read8(self.ea_direct(zp));
                let res = self.r.b | v;
                self.logic8_nz_clearv(res);
                self.r.b = res;
                consumed = 4;
                if trace {
                    println!("{:04X}: DA {:02X}   ORB ${:02X}", pc0, zp, zp);
                }
            }
            0xDB => {
                let zp = self.fetch8(bus);
                let v = bus.read8(self.ea_direct(zp));
                let b = self.r.b;
                self.r.b = self.add8_flags(b, v);
                consumed = 4;
                if trace {
                    println!("{:04X}: DB {:02X}   ADDB ${:02X}", pc0, zp, zp);
                }
            }
            // Indexed B (0xE0..)
            0xE0 => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                self.r.b = self.sub8_flags(self.r.b, v);
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: E0 ..    SUBB {}", pc0, desc);
                }
            }
            0xE1 => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let b = self.r.b;
                let _ = self.sub8_flags(b, v);
                self.r.b = b;
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: E1 ..    CMPB {}", pc0, desc);
                }
            }
            0xE2 => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let b = self.r.b;
                self.r.b = self.sbc8_flags(b, v);
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: E2 ..    SBCB {}", pc0, desc);
                }
            }
            0xE4 => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let res = self.r.b & v;
                self.logic8_nz_clearv(res);
                self.r.b = res;
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: E4 ..    ANDB {}", pc0, desc);
                }
            }
            0xE5 => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let res = self.r.b & v;
                self.logic8_nz_clearv(res);
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: E5 ..    BITB {}", pc0, desc);
                }
            }
            0xE6 => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                self.r.b = v;
                self.set_nz8(v);
                self.r.cc &= !V;
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: E6 ..    LDB {}", pc0, desc);
                }
            }
            0xE7 => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                bus.write8(ea, self.r.b);
                self.set_nz8(self.r.b);
                self.r.cc &= !V;
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: E7 ..    STB {}", pc0, desc);
                }
            }
            0xE8 => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let res = self.r.b ^ v;
                self.logic8_nz_clearv(res);
                self.r.b = res;
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: E8 ..    EORB {}", pc0, desc);
                }
            }
            0xE9 => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let b = self.r.b;
                self.r.b = self.adc8_flags(b, v);
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: E9 ..    ADCB {}", pc0, desc);
                }
            }
            0xEA => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let res = self.r.b | v;
                self.logic8_nz_clearv(res);
                self.r.b = res;
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: EA ..    ORB {}", pc0, desc);
                }
            }
            0xEB => {
                let (ea, ac, desc) = self.ea_indexed(bus);
                let v = bus.read8(ea);
                let b = self.r.b;
                self.r.b = self.add8_flags(b, v);
                consumed = 4 + ac;
                if trace {
                    println!("{:04X}: EB ..    ADDB {}", pc0, desc);
                }
            }
            // Extended B (0xF0..)
            0xF0 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                self.r.b = self.sub8_flags(self.r.b, v);
                consumed = 5;
                if trace {
                    println!("{:04X}: F0 {:04X} SUBB ${:04X}", pc0, addr, addr);
                }
            }
            0xF1 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let b = self.r.b;
                let _ = self.sub8_flags(b, v);
                self.r.b = b;
                consumed = 5;
                if trace {
                    println!("{:04X}: F1 {:04X} CMPB ${:04X}", pc0, addr, addr);
                }
            }
            0xF2 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let b = self.r.b;
                self.r.b = self.sbc8_flags(b, v);
                consumed = 5;
                if trace {
                    println!("{:04X}: F2 {:04X} SBCB ${:04X}", pc0, addr, addr);
                }
            }
            0xF4 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let res = self.r.b & v;
                self.logic8_nz_clearv(res);
                self.r.b = res;
                consumed = 5;
                if trace {
                    println!("{:04X}: F4 {:04X} ANDB ${:04X}", pc0, addr, addr);
                }
            }
            0xF5 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let res = self.r.b & v;
                self.logic8_nz_clearv(res);
                consumed = 5;
                if trace {
                    println!("{:04X}: F5 {:04X} BITB ${:04X}", pc0, addr, addr);
                }
            }
            0xF6 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                self.r.b = v;
                self.set_nz8(v);
                self.r.cc &= !V;
                consumed = 5;
                if trace {
                    println!("{:04X}: F6 {:04X} LDB ${:04X}", pc0, addr, addr);
                }
            }
            0xF7 => {
                let addr = self.ea_extended(bus);
                bus.write8(addr, self.r.b);
                self.set_nz8(self.r.b);
                self.r.cc &= !V;
                consumed = 5;
                if trace {
                    println!("{:04X}: F7 {:04X} STB ${:04X}", pc0, addr, addr);
                }
            }
            0xF8 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let res = self.r.b ^ v;
                self.logic8_nz_clearv(res);
                self.r.b = res;
                consumed = 5;
                if trace {
                    println!("{:04X}: F8 {:04X} EORB ${:04X}", pc0, addr, addr);
                }
            }
            0xF9 => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let b = self.r.b;
                self.r.b = self.adc8_flags(b, v);
                consumed = 5;
                if trace {
                    println!("{:04X}: F9 {:04X} ADCB ${:04X}", pc0, addr, addr);
                }
            }
            0xFA => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let res = self.r.b | v;
                self.logic8_nz_clearv(res);
                self.r.b = res;
                consumed = 5;
                if trace {
                    println!("{:04X}: FA {:04X} ORB ${:04X}", pc0, addr, addr);
                }
            }
            0xFB => {
                let addr = self.ea_extended(bus);
                let v = bus.read8(addr);
                let b = self.r.b;
                self.r.b = self.add8_flags(b, v);
                consumed = 5;
                if trace {
                    println!("{:04X}: FB {:04X} ADDB ${:04X}", pc0, addr, addr);
                }
            }
            0x8B => {
                // ADDA #imm
                let imm = self.fetch8(bus);
                let a = self.r.a;
                let (res8, carry1) = a.overflowing_add(imm);
                let half = ((a & 0x0F) + (imm & 0x0F)) > 0x0F;
                let ovf = ((a ^ res8) & (imm ^ res8) & 0x80) != 0;
                self.r.a = res8;
                self.set_nz8(res8);
                self.r.cc = (self.r.cc & !(C | V | H))
                    | (if carry1 { C } else { 0 })
                    | (if ovf { V } else { 0 })
                    | (if half { H } else { 0 });
                consumed = 2;
                if trace {
                    println!("{:04X}: 8B {:02X}   ADDA #${:02X}", pc0, imm, imm);
                }
            }
            0xAE => {
                // LDX indexed
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                let hi = bus.read8(ea) as u16;
                let lo = bus.read8(ea.wrapping_add(1)) as u16;
                let v = (hi << 8) | lo;
                self.r.x = v;
                self.set_nz16(v);
                self.r.cc &= !V;
                consumed = 5 + addcyc;
                if trace {
                    println!("{:04X}: AE ..    LDX {} -> {:04X}", pc0, desc, ea);
                }
            }
            0xAF => {
                // STX indexed
                let (ea, addcyc, desc) = self.ea_indexed(bus);
                bus.write8(ea, (self.r.x >> 8) as u8);
                bus.write8(ea.wrapping_add(1), (self.r.x & 0xFF) as u8);
                self.set_nz16(self.r.x);
                self.r.cc &= !V;
                consumed = 5 + addcyc;
                if trace {
                    println!("{:04X}: AF ..    STX {} -> {:04X}", pc0, desc, ea);
                }
            }
            _ => {
                // For now, treat unimplemented opcode as NOP-like to avoid hang
                if trace {
                    println!("{:04X}: {:02X}      UNIMPL", pc0, op);
                }
                consumed = 1; // placeholder
            }
        }
        self.cycles += consumed as u64;
        // Poll bus interrupt lines
        let (nmi_line, firq_line, irq_line) = bus.irq_lines();
        if nmi_line {
            self.nmi_pending = true;
        }
        if firq_line {
            self.firq_pending = true;
        }
        if irq_line {
            self.irq_pending = true;
        }
        // After instruction, handle interrupts
        let mut ic = 0u32;
        if self.nmi_pending {
            ic += self.service_nmi(bus);
        } else if self.firq_pending && (self.r.cc & F) == 0 {
            ic += self.service_firq(bus);
        } else if self.irq_pending && (self.r.cc & I) == 0 {
            ic += self.service_irq(bus);
        }
        self.cycles += ic as u64;
        consumed + ic
    }
}
pub fn regs_snapshot(cpu: &Cpu) -> Registers {
    cpu.r
}
