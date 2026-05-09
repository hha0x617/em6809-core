//! # `debug` — debugger primitives
//!
//! Everything the GUI's debugger surface needs that isn't on the CPU
//! itself: breakpoints (with conditional expression evaluator),
//! shadow call stack, instruction-boundary tracking, and a small set
//! of memory/register dump helpers.
//!
//! The [`crate::cpu::Cpu`] embeds a [`ShadowCallStack`] and consults
//! a [`BreakpointSet`] every step, so embedders typically just
//! configure these structures and let the CPU do the bookkeeping.
//!
//! ## Provided types
//!
//! ### Breakpoints
//!
//! - [`BreakpointId`] — opaque newtype around `u32`.  Returned by
//!   [`BreakpointSet::add`] and used to identify breakpoints in
//!   subsequent calls.
//! - [`Breakpoint`] — `pub address: u16`, `pub enabled: bool`,
//!   `pub condition: Option<String>`, `pub hit_count: u64`,
//!   `pub ignore_count: u64`.  The `condition` is a tiny C-style
//!   expression language (see the docstring on [`BreakpointSet`])
//!   with `==`, `!=`, `<`, `<=`, `>`, `>=`, `&&`, `||`, `!`, `+`,
//!   `-`, `*`, `&`, `|`, `^`, parentheses, hex (`0x..` / `$..`),
//!   decimal, register names (`a`/`b`/`d`/`x`/`y`/`u`/`s`/`pc`/
//!   `dp`/`cc`).
//! - [`BreakpointSet`] — owns the `Vec<Breakpoint>`.  Methods:
//!   - `add(addr) -> BreakpointId` / `remove(id)` /
//!     `set_enabled(id, bool)` / `set_condition(id, Option<String>)`.
//!   - `should_break(pc)` — fast pre-step check the CPU calls
//!     before evaluating conditions (skips over hit-count, etc.).
//!   - `check(pc, &Registers)` — full check including condition
//!     evaluation; called when `should_break` says yes.
//!   - `iter()` / `len()` for UI rendering.
//!
//! ### Shadow call stack
//!
//! - [`CallKind`] — what pushed this frame (`Bsr` / `Lbsr` / `Jsr`
//!   / `Swi` / `Irq` / `Firq` / `Nmi`).
//! - [`CallFrame`] — `pub return_addr: u16`, `pub kind: CallKind`,
//!   plus a few register snapshots for UI display.
//! - [`ShadowCallStack`] — append-only `Vec<CallFrame>` updated by
//!   the CPU's call/return paths.  `frames()`, `top()`, `depth()` for
//!   read access; the CPU pushes/pops directly.
//!
//! ### Instruction boundaries
//!
//! - [`InstructionBoundaries`] — set of address ranges known to start
//!   instruction boundaries.  Lets the GUI's listing pane scroll
//!   without landing in the middle of a multi-byte op.
//! - [`linear_sweep`] — populate an `InstructionBoundaries` from a
//!   linear walk of an address range.
//!
//! ## Free dump helpers
//!
//! - [`dump_registers`] — print `&Cpu` to stdout (CLI/test usage).
//! - [`dump_memory`] / [`dump_memory_bus`] / [`dump_memory_ascii`] —
//!   hex / hex+ASCII memory dumps for a `&Memory` or any `&mut Bus`.
//!
//! ## Typical usage
//!
//! ```no_run
//! use em6809_core::bus::Memory;
//! use em6809_core::cpu::Cpu;
//! use em6809_core::debug::BreakpointSet;
//!
//! let mut bus = Memory::new();
//! let mut cpu = Cpu::new();
//! let mut bps = BreakpointSet::default();
//! let id = bps.add(0x1234);
//! bps.set_condition(id, Some("a == 0x42 && pc < $2000".into()));
//!
//! cpu.reset(&mut bus);
//! loop {
//!     if let Some(hit) = bps.check(cpu.r.pc, &cpu.r) {
//!         println!("stopped on bp {:?}", hit);
//!         break;
//!     }
//!     cpu.step(&mut bus, false);
//! }
//! ```

#![allow(clippy::uninlined_format_args)]
use std::collections::BTreeSet;

use crate::bus::{Bus, Memory};
use crate::cpu::{Cpu, Registers};
use crate::disasm::disasm_one;

// =============================================================================
// Breakpoint condition expression language
// =============================================================================
//
// Tiny calculator-style expression language used by `Breakpoint::condition`.
// Recognised tokens are arithmetic-and-comparison friendly so users can write
// the kinds of guards they would naturally type into a debugger:
//
//   a == 0x42
//   pc >= $1000 && pc < $2000
//   x > 0 && y != x
//   !(d == 0)
//   a + b == 0xFF
//
// Grammar (loosely; recursive-descent, left-associative for binary ops at the
// same precedence level):
//
//   expr     := or_expr
//   or_expr  := and_expr   ( "||"  and_expr  )*
//   and_expr := not_expr   ( "&&"  not_expr  )*
//   not_expr := "!" not_expr | rel_expr
//   rel_expr := add_expr   ( cmp_op  add_expr )?         (no chaining)
//   add_expr := mul_expr   ( ("+" | "-") mul_expr )*
//   mul_expr := unary      ( ("*" | "/") unary    )*
//   unary    := "-" unary  | atom
//   atom     := number | register | "(" expr ")"
//
//   number   := decimal | "0x" hex | "$" hex
//   register := a|b|d|x|y|u|s|pc|dp|cc      (case-insensitive)
//
// Boolean values are represented as 0/1 i64 so arithmetic and logical ops
// compose without a separate value type. The final result of `evaluate` is
// `result != 0`. There is no memory access (`[addr]`) or CC-bit accessor in
// this v1; both are queued for a follow-up.
pub mod cond {
    use super::Registers;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum CondError {
        /// Tokenizer hit a character it doesn't know about.
        UnknownChar(char),
        /// A token sequence didn't match the grammar.
        Parse(String),
        /// Successful parse, but evaluation hit something unrecoverable
        /// (currently only "divide by zero").
        DivideByZero,
        /// The string `regs.a`-style would need a CC-bit accessor or
        /// memory read that this v1 doesn't support yet. Reserved.
        Unsupported(String),
    }

    impl std::fmt::Display for CondError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                CondError::UnknownChar(c) => write!(f, "unknown character '{}'", c),
                CondError::Parse(msg) => write!(f, "parse error: {}", msg),
                CondError::DivideByZero => write!(f, "divide by zero"),
                CondError::Unsupported(s) => write!(f, "unsupported: {}", s),
            }
        }
    }

    type Result<T> = std::result::Result<T, CondError>;

    #[derive(Debug, Clone, PartialEq)]
    enum Tok {
        Num(i64),
        Ident(String),
        Eq,
        Ne,
        Lt,
        Le,
        Gt,
        Ge,
        AndAnd,
        OrOr,
        Bang,
        Plus,
        Minus,
        Star,
        Slash,
        LParen,
        RParen,
    }

    fn tokenize(s: &str) -> Result<Vec<Tok>> {
        let bytes = s.as_bytes();
        let mut i = 0;
        let mut out = Vec::new();
        while i < bytes.len() {
            let c = bytes[i] as char;
            if c.is_whitespace() {
                i += 1;
                continue;
            }
            // Numeric literal: 0x.., $.., or decimal.
            if c == '$' {
                i += 1;
                let start = i;
                while i < bytes.len() && (bytes[i] as char).is_ascii_hexdigit() {
                    i += 1;
                }
                if start == i {
                    return Err(CondError::Parse("expected hex digits after '$'".into()));
                }
                let v = i64::from_str_radix(&s[start..i], 16)
                    .map_err(|e| CondError::Parse(e.to_string()))?;
                out.push(Tok::Num(v));
                continue;
            }
            if c == '0' && i + 1 < bytes.len() && (bytes[i + 1] as char) == 'x' {
                i += 2;
                let start = i;
                while i < bytes.len() && (bytes[i] as char).is_ascii_hexdigit() {
                    i += 1;
                }
                if start == i {
                    return Err(CondError::Parse("expected hex digits after '0x'".into()));
                }
                let v = i64::from_str_radix(&s[start..i], 16)
                    .map_err(|e| CondError::Parse(e.to_string()))?;
                out.push(Tok::Num(v));
                continue;
            }
            if c.is_ascii_digit() {
                let start = i;
                while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                    i += 1;
                }
                let v: i64 = s[start..i]
                    .parse()
                    .map_err(|e: std::num::ParseIntError| CondError::Parse(e.to_string()))?;
                out.push(Tok::Num(v));
                continue;
            }
            // Identifier (register name).
            if c.is_ascii_alphabetic() || c == '_' {
                let start = i;
                while i < bytes.len() && {
                    let ch = bytes[i] as char;
                    ch.is_ascii_alphanumeric() || ch == '_'
                } {
                    i += 1;
                }
                out.push(Tok::Ident(s[start..i].to_ascii_lowercase()));
                continue;
            }
            // Multi-character operators first.
            let two: &str = if i + 1 < bytes.len() {
                &s[i..i + 2]
            } else {
                ""
            };
            match two {
                "==" => {
                    out.push(Tok::Eq);
                    i += 2;
                    continue;
                }
                "!=" => {
                    out.push(Tok::Ne);
                    i += 2;
                    continue;
                }
                "<=" => {
                    out.push(Tok::Le);
                    i += 2;
                    continue;
                }
                ">=" => {
                    out.push(Tok::Ge);
                    i += 2;
                    continue;
                }
                "&&" => {
                    out.push(Tok::AndAnd);
                    i += 2;
                    continue;
                }
                "||" => {
                    out.push(Tok::OrOr);
                    i += 2;
                    continue;
                }
                _ => {}
            }
            // Single-character punctuation.
            let tok = match c {
                '<' => Tok::Lt,
                '>' => Tok::Gt,
                '!' => Tok::Bang,
                '+' => Tok::Plus,
                '-' => Tok::Minus,
                '*' => Tok::Star,
                '/' => Tok::Slash,
                '(' => Tok::LParen,
                ')' => Tok::RParen,
                other => return Err(CondError::UnknownChar(other)),
            };
            out.push(tok);
            i += 1;
        }
        Ok(out)
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Reg {
        A,
        B,
        D,
        X,
        Y,
        U,
        S,
        Pc,
        Dp,
        Cc,
    }

    fn reg_from_ident(name: &str) -> Result<Reg> {
        match name {
            "a" => Ok(Reg::A),
            "b" => Ok(Reg::B),
            "d" => Ok(Reg::D),
            "x" => Ok(Reg::X),
            "y" => Ok(Reg::Y),
            "u" => Ok(Reg::U),
            "s" => Ok(Reg::S),
            "pc" => Ok(Reg::Pc),
            "dp" => Ok(Reg::Dp),
            "cc" => Ok(Reg::Cc),
            other => Err(CondError::Parse(format!("unknown identifier '{}'", other))),
        }
    }

    fn reg_value(r: Reg, regs: &Registers) -> i64 {
        match r {
            Reg::A => regs.a as i64,
            Reg::B => regs.b as i64,
            Reg::D => (((regs.a as u16) << 8) | (regs.b as u16)) as i64,
            Reg::X => regs.x as i64,
            Reg::Y => regs.y as i64,
            Reg::U => regs.u as i64,
            Reg::S => regs.s as i64,
            Reg::Pc => regs.pc as i64,
            Reg::Dp => regs.dp as i64,
            Reg::Cc => regs.cc as i64,
        }
    }

    #[derive(Debug, Clone)]
    enum Expr {
        Num(i64),
        Reg(Reg),
        Neg(Box<Expr>),
        Not(Box<Expr>),
        Bin(BinOp, Box<Expr>, Box<Expr>),
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum BinOp {
        Add,
        Sub,
        Mul,
        Div,
        Eq,
        Ne,
        Lt,
        Le,
        Gt,
        Ge,
        And,
        Or,
    }

    struct Parser {
        toks: Vec<Tok>,
        pos: usize,
    }

    impl Parser {
        fn peek(&self) -> Option<&Tok> {
            self.toks.get(self.pos)
        }
        fn bump(&mut self) -> Option<Tok> {
            let t = self.toks.get(self.pos).cloned();
            if t.is_some() {
                self.pos += 1;
            }
            t
        }
        fn parse_expr(&mut self) -> Result<Expr> {
            let e = self.parse_or()?;
            if self.pos != self.toks.len() {
                return Err(CondError::Parse(format!(
                    "trailing tokens after expression at position {}",
                    self.pos
                )));
            }
            Ok(e)
        }
        fn parse_or(&mut self) -> Result<Expr> {
            let mut lhs = self.parse_and()?;
            while matches!(self.peek(), Some(Tok::OrOr)) {
                self.bump();
                let rhs = self.parse_and()?;
                lhs = Expr::Bin(BinOp::Or, Box::new(lhs), Box::new(rhs));
            }
            Ok(lhs)
        }
        fn parse_and(&mut self) -> Result<Expr> {
            let mut lhs = self.parse_not()?;
            while matches!(self.peek(), Some(Tok::AndAnd)) {
                self.bump();
                let rhs = self.parse_not()?;
                lhs = Expr::Bin(BinOp::And, Box::new(lhs), Box::new(rhs));
            }
            Ok(lhs)
        }
        fn parse_not(&mut self) -> Result<Expr> {
            if matches!(self.peek(), Some(Tok::Bang)) {
                self.bump();
                let e = self.parse_not()?;
                return Ok(Expr::Not(Box::new(e)));
            }
            self.parse_rel()
        }
        fn parse_rel(&mut self) -> Result<Expr> {
            let lhs = self.parse_add()?;
            let op = match self.peek() {
                Some(Tok::Eq) => BinOp::Eq,
                Some(Tok::Ne) => BinOp::Ne,
                Some(Tok::Lt) => BinOp::Lt,
                Some(Tok::Le) => BinOp::Le,
                Some(Tok::Gt) => BinOp::Gt,
                Some(Tok::Ge) => BinOp::Ge,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.parse_add()?;
            Ok(Expr::Bin(op, Box::new(lhs), Box::new(rhs)))
        }
        fn parse_add(&mut self) -> Result<Expr> {
            let mut lhs = self.parse_mul()?;
            loop {
                let op = match self.peek() {
                    Some(Tok::Plus) => BinOp::Add,
                    Some(Tok::Minus) => BinOp::Sub,
                    _ => break,
                };
                self.bump();
                let rhs = self.parse_mul()?;
                lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
            }
            Ok(lhs)
        }
        fn parse_mul(&mut self) -> Result<Expr> {
            let mut lhs = self.parse_unary()?;
            loop {
                let op = match self.peek() {
                    Some(Tok::Star) => BinOp::Mul,
                    Some(Tok::Slash) => BinOp::Div,
                    _ => break,
                };
                self.bump();
                let rhs = self.parse_unary()?;
                lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
            }
            Ok(lhs)
        }
        fn parse_unary(&mut self) -> Result<Expr> {
            if matches!(self.peek(), Some(Tok::Minus)) {
                self.bump();
                let e = self.parse_unary()?;
                return Ok(Expr::Neg(Box::new(e)));
            }
            self.parse_atom()
        }
        fn parse_atom(&mut self) -> Result<Expr> {
            match self.bump() {
                Some(Tok::Num(n)) => Ok(Expr::Num(n)),
                Some(Tok::Ident(name)) => Ok(Expr::Reg(reg_from_ident(&name)?)),
                Some(Tok::LParen) => {
                    let e = self.parse_or()?;
                    match self.bump() {
                        Some(Tok::RParen) => Ok(e),
                        _ => Err(CondError::Parse("expected ')'".into())),
                    }
                }
                other => Err(CondError::Parse(format!(
                    "expected number, register, or '(' but got {:?}",
                    other
                ))),
            }
        }
    }

    fn parse(s: &str) -> Result<Expr> {
        let toks = tokenize(s)?;
        if toks.is_empty() {
            return Err(CondError::Parse("empty expression".into()));
        }
        let mut p = Parser { toks, pos: 0 };
        p.parse_expr()
    }

    fn eval(e: &Expr, regs: &Registers) -> Result<i64> {
        Ok(match e {
            Expr::Num(n) => *n,
            Expr::Reg(r) => reg_value(*r, regs),
            Expr::Neg(x) => -eval(x, regs)?,
            Expr::Not(x) => {
                if eval(x, regs)? == 0 {
                    1
                } else {
                    0
                }
            }
            Expr::Bin(op, a, b) => {
                let av = eval(a, regs)?;
                let bv = eval(b, regs)?;
                match op {
                    BinOp::Add => av.wrapping_add(bv),
                    BinOp::Sub => av.wrapping_sub(bv),
                    BinOp::Mul => av.wrapping_mul(bv),
                    BinOp::Div => {
                        if bv == 0 {
                            return Err(CondError::DivideByZero);
                        }
                        av.wrapping_div(bv)
                    }
                    BinOp::Eq => (av == bv) as i64,
                    BinOp::Ne => (av != bv) as i64,
                    BinOp::Lt => (av < bv) as i64,
                    BinOp::Le => (av <= bv) as i64,
                    BinOp::Gt => (av > bv) as i64,
                    BinOp::Ge => (av >= bv) as i64,
                    BinOp::And => ((av != 0) && (bv != 0)) as i64,
                    BinOp::Or => ((av != 0) || (bv != 0)) as i64,
                }
            }
        })
    }

    /// Parse and evaluate `s` against `regs`. Returns whether the
    /// resulting numeric value is non-zero. Empty / whitespace-only
    /// strings parse as an error; the caller (`BreakpointSet::check`)
    /// distinguishes "no condition" *before* it ever calls this.
    pub fn evaluate(s: &str, regs: &Registers) -> Result<bool> {
        let ast = parse(s)?;
        let v = eval(&ast, regs)?;
        Ok(v != 0)
    }
}

pub use cond::CondError;

// =============================================================================
// Instruction boundaries
// =============================================================================
//
// MC6809 instructions are 1..5 bytes long. Decoding `bus[pc]` as if it were
// always an opcode goes well only when `pc` happens to be the start of a real
// instruction; offset by one byte and the entire downstream stream is wrong.
// That's why disassembly views that anchor on `pc - some_constant` look
// stable until the user reaches a region where the chosen offset isn't a
// boundary, and then everything desyncs — including the current-PC line and
// any breakpoint markers, since those are matched by address against the
// (now-misaligned) line list.
//
// `InstructionBoundaries` is a confirmed-boundary set: every address it
// holds is *known* to be an instruction start, either because the CPU has
// actually executed that PC or because a linear sweep of a known code
// region disassembled an instruction starting there. Disassembly views
// then anchor on `boundaries.floor(pc)` — the most recent confirmed
// boundary at or before `pc` — guaranteeing the rendered listing aligns
// with the real instruction stream around `pc`.

/// A set of addresses that are *known* to be instruction starts.
///
/// Used by disassembly views to pick a safe anchor near the current PC
/// instead of guessing with `pc - constant` arithmetic. Populate via
/// [`InstructionBoundaries::insert`] (per executed instruction) and
/// [`linear_sweep`] (after loading code).
#[derive(Debug, Default, Clone)]
pub struct InstructionBoundaries {
    set: BTreeSet<u16>,
}

impl InstructionBoundaries {
    pub fn new() -> Self {
        Self {
            set: BTreeSet::new(),
        }
    }

    /// Forget every recorded boundary. Call this when the program is
    /// reloaded, the bus is reset, or memory contents change in a way
    /// that invalidates previously-confirmed instruction starts.
    pub fn clear(&mut self) {
        self.set.clear();
    }

    /// Record `addr` as an instruction start.
    pub fn insert(&mut self, addr: u16) {
        self.set.insert(addr);
    }

    pub fn extend<I: IntoIterator<Item = u16>>(&mut self, iter: I) {
        self.set.extend(iter);
    }

    /// Largest known boundary `<= addr`. `None` if no boundary at or
    /// below `addr` has been seen yet.
    pub fn floor(&self, addr: u16) -> Option<u16> {
        self.set.range(..=addr).next_back().copied()
    }

    /// Smallest known boundary `>= addr`. `None` if no boundary at or
    /// above `addr` has been seen yet.
    pub fn ceil(&self, addr: u16) -> Option<u16> {
        self.set.range(addr..).next().copied()
    }

    /// Iterate boundaries in ascending order over the inclusive range
    /// `[from, to]`. Implements `DoubleEndedIterator` so callers can
    /// walk the range from either side via `.rev()`.
    pub fn iter_range(&self, from: u16, to: u16) -> impl DoubleEndedIterator<Item = u16> + '_ {
        self.set.range(from..=to).copied()
    }

    pub fn contains(&self, addr: u16) -> bool {
        self.set.contains(&addr)
    }

    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
}

/// Disassemble linearly from `start` up to (but not including) `end`,
/// recording each instruction start in `boundaries`.
///
/// This is the cheap pass to run right after a program load — it walks
/// the byte stream as if every encountered byte starts an instruction,
/// which is correct for pure code regions. In code/data mixed regions
/// the sweep may stamp boundaries onto bytes that are actually data;
/// the resulting "phantom" boundaries do no harm as long as the
/// real PC ends up inside a true code region, because
/// [`InstructionBoundaries::floor`] will return whichever true boundary
/// is closest.
///
/// Stops when `pc >= end` or when it would wrap past 0xFFFF.
pub fn linear_sweep<B: Bus + ?Sized>(
    bus: &mut B,
    start: u16,
    end: u16,
    boundaries: &mut InstructionBoundaries,
) {
    if end <= start {
        return;
    }
    let mut pc = start;
    while pc < end {
        boundaries.insert(pc);
        let (consumed, _) = disasm_one(bus, pc);
        // disasm_one always consumes at least one byte for valid 6809;
        // guard against pathological zero-consume to avoid infinite loop.
        if consumed == 0 {
            break;
        }
        let (next, wrapped) = pc.overflowing_add(consumed);
        if wrapped || next <= pc {
            break;
        }
        pc = next;
    }
}

// =============================================================================
// Shadow call stack
// =============================================================================
//
// MC6809 has no software-agnostic frame marker — each subroutine writes only a
// 16-bit return address to the S stack, and what (if anything) sits below it
// (saved registers, local frame, alloca-style scratch) is purely a convention
// of the calling code. That makes "walk the S stack" unreliable for a debugger
// view, especially across compilers and hand-written code that uses U/S flexibly.
//
// Instead, the CPU itself tells us where each frame begins, by virtue of being
// the thing that just executed `BSR`/`JSR`/`LBSR`/`SWI`/etc. We hook those
// instructions and the interrupt-entry/exit paths in `cpu.rs`, recording
// frames here as they happen. RTS/RTI pops them. The result is a frame list
// that's accurate by construction: it can't desync with the real stack
// unless the program does something exotic (manual stack edits, longjmp).
//
// `sp_at_call` is the S register *after* the return address was pushed —
// i.e., where S points when the callee's prologue runs. Storing it lets a UI
// (or a future "step out" implementation) sanity-check pops by comparing the
// popped frame's `sp_at_call` against the current S, and gracefully drop
// frames that the program has unwound through some non-RTS path.

/// What kind of control transfer caused a frame to be pushed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    /// Branch to subroutine — `BSR rel8` / `LBSR rel16`.
    Bsr,
    /// Jump to subroutine — `JSR ext` / `JSR direct` / `JSR indexed`.
    Jsr,
    /// Software interrupt. The `u8` is 1, 2, or 3 (`SWI` / `SWI2` / `SWI3`).
    Swi(u8),
    /// Maskable interrupt vectored via `$FFF8/$FFF9`.
    Irq,
    /// Fast interrupt vectored via `$FFF6/$FFF7`.
    Firq,
    /// Non-maskable interrupt vectored via `$FFFC/$FFFD`.
    Nmi,
}

/// One entry on the shadow call stack.
#[derive(Debug, Clone, Copy)]
pub struct CallFrame {
    /// PC pushed onto S — i.e., the address `RTS` will jump back to.
    pub return_addr: u16,
    /// PC of the call/branch/SWI instruction itself. Useful for "you came
    /// from here" annotations in the UI.
    pub call_site: u16,
    /// PC of the entry into the callee (= `cpu.r.pc` immediately after the
    /// call instruction completes, before the first instruction of the
    /// callee runs).
    pub target: u16,
    /// `S` register value right after the return address was pushed.
    pub sp_at_call: u16,
    /// Why the frame was pushed.
    pub kind: CallKind,
}

/// Stack of `CallFrame`s, populated on call/interrupt entry and drained on
/// `RTS`/`RTI`. Owned by `Cpu` and updated from the relevant instruction
/// handlers in `cpu.rs`.
#[derive(Debug, Default, Clone)]
pub struct ShadowCallStack {
    frames: Vec<CallFrame>,
}

impl ShadowCallStack {
    pub fn new() -> Self {
        Self { frames: Vec::new() }
    }

    /// Forget every recorded frame. Call on `cpu.reset()` and on warm
    /// reboots; otherwise stale frames from before the reset would
    /// outlive the program that produced them.
    pub fn clear(&mut self) {
        self.frames.clear();
    }

    pub fn push(&mut self, frame: CallFrame) {
        self.frames.push(frame);
    }

    /// Pop the top frame, but only if it looks like the caller actually
    /// returned through it: `RTS` increments S by 2, so `current_sp`
    /// (S after the pull) should equal `top.sp_at_call + 2`
    /// (S right after the original push). If the numbers don't line
    /// up the program unwound through some non-RTS path (e.g.,
    /// `puls pc`-style manual return after extra pushes); drop frames
    /// until we find one that lines up, or the stack is empty.
    ///
    /// Returns the frame that was popped, or `None` if no frame matched.
    pub fn pop_for_return(&mut self, current_sp: u16) -> Option<CallFrame> {
        while let Some(top) = self.frames.last() {
            if top.sp_at_call.wrapping_add(2) == current_sp {
                return self.frames.pop();
            }
            // Top frame's S no longer matches — the program returned
            // through some other path. Drop it and try the next one.
            self.frames.pop();
        }
        None
    }

    /// Same as `pop_for_return` but without the SP-equality guard.
    /// Used by `RTI`, where the interrupt frame's saved CC/regs make the
    /// SP arithmetic less straightforward; matching by call-kind is more
    /// robust there. Pops the topmost frame whose kind is
    /// `Swi(_)` / `Irq` / `Firq` / `Nmi`.
    pub fn pop_for_rti(&mut self) -> Option<CallFrame> {
        while let Some(top) = self.frames.last().copied() {
            self.frames.pop();
            if matches!(
                top.kind,
                CallKind::Swi(_) | CallKind::Irq | CallKind::Firq | CallKind::Nmi
            ) {
                return Some(top);
            }
        }
        None
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Frames in push order — index 0 is the outermost call, the last
    /// element is the current callee.
    pub fn frames(&self) -> &[CallFrame] {
        &self.frames
    }

    /// Top frame without popping.
    pub fn peek(&self) -> Option<&CallFrame> {
        self.frames.last()
    }
}

// =============================================================================
// Breakpoints
// =============================================================================
//
// Address-execution breakpoints with the standard set of attributes a
// user-facing debugger needs: enable/disable, hit count, ignore count,
// and a free-form condition string. The condition is *stored* here but
// not evaluated yet — leaving the evaluator to a follow-up phase keeps
// this PR focused, and the field shape already matches the emfe ABI's
// `EmfeBreakpointInfo {address, enabled, condition}` so no churn is
// expected when condition evaluation lands.
//
// Breakpoints are referenced from outside by an opaque `BreakpointId`
// so callers don't need to know the internal storage strategy. IDs are
// monotonically increasing and never reused — that way a UI element
// holding an old ID after the BP was removed gets a clean
// `ERR_NOTFOUND` instead of accidentally controlling a different BP.

/// Stable identifier for a breakpoint, returned by `add` and used by
/// every other operation. IDs are dense small integers but treat them
/// as opaque — implementation may switch to nonces later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BreakpointId(pub u32);

#[derive(Debug, Clone)]
pub struct Breakpoint {
    pub id: BreakpointId,
    pub addr: u16,
    pub enabled: bool,
    /// Optional condition string. **Currently stored but not evaluated.**
    /// When `Some(_)`, `should_break` still returns true on every hit;
    /// future work will plug in an expression evaluator here.
    pub condition: Option<String>,
    /// Number of times this breakpoint has actually caused a stop.
    pub hit_count: u64,
    /// Number of remaining hits to silently skip before the breakpoint
    /// next causes a stop. Decrement-and-skip happens inside
    /// `should_break`; the user-visible hit_count only advances when a
    /// stop is actually returned.
    pub ignore_count: u64,
}

#[derive(Debug, Default, Clone)]
pub struct BreakpointSet {
    bps: Vec<Breakpoint>,
    next_id: u32,
}

impl BreakpointSet {
    pub fn new() -> Self {
        Self {
            bps: Vec::new(),
            next_id: 1,
        }
    }

    /// Add a new execution breakpoint at `addr`. Returns the new ID.
    /// Adding multiple breakpoints at the same address is allowed (each
    /// gets its own ID, ignore/hit counters, and condition).
    pub fn add(&mut self, addr: u16) -> BreakpointId {
        let id = BreakpointId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.bps.push(Breakpoint {
            id,
            addr,
            enabled: true,
            condition: None,
            hit_count: 0,
            ignore_count: 0,
        });
        id
    }

    pub fn remove(&mut self, id: BreakpointId) -> bool {
        if let Some(idx) = self.bps.iter().position(|b| b.id == id) {
            self.bps.remove(idx);
            true
        } else {
            false
        }
    }

    pub fn set_enabled(&mut self, id: BreakpointId, enabled: bool) -> bool {
        if let Some(b) = self.bps.iter_mut().find(|b| b.id == id) {
            b.enabled = enabled;
            true
        } else {
            false
        }
    }

    pub fn set_condition(&mut self, id: BreakpointId, condition: Option<String>) -> bool {
        if let Some(b) = self.bps.iter_mut().find(|b| b.id == id) {
            b.condition = condition;
            true
        } else {
            false
        }
    }

    pub fn set_ignore_count(&mut self, id: BreakpointId, n: u64) -> bool {
        if let Some(b) = self.bps.iter_mut().find(|b| b.id == id) {
            b.ignore_count = n;
            true
        } else {
            false
        }
    }

    pub fn clear(&mut self) {
        self.bps.clear();
    }

    pub fn len(&self) -> usize {
        self.bps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bps.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Breakpoint> {
        self.bps.iter()
    }

    /// Decide whether `pc` triggers a stop, ignoring conditions.
    /// See `check` for the full version that consults
    /// `Breakpoint::condition`. Kept for callers (and tests) that
    /// don't have a register snapshot handy and want the
    /// pre-condition behaviour.
    ///
    /// - Disabled BPs are skipped entirely.
    /// - If `ignore_count > 0`, decrement it and pretend this BP didn't
    ///   match (other BPs at the same address may still trigger).
    /// - Otherwise, increment `hit_count` and remember this BP as the
    ///   trigger.
    ///
    /// Returns `Some(id)` of the first BP that actually caused a stop,
    /// or `None` if no enabled, non-skipped BP matched. The `id` lets
    /// the run loop report which breakpoint hit, for UI labeling.
    pub fn should_break(&mut self, pc: u16) -> Option<BreakpointId> {
        let mut hit: Option<BreakpointId> = None;
        for b in self.bps.iter_mut() {
            if !b.enabled || b.addr != pc {
                continue;
            }
            if b.ignore_count > 0 {
                b.ignore_count -= 1;
                continue;
            }
            b.hit_count = b.hit_count.saturating_add(1);
            if hit.is_none() {
                hit = Some(b.id);
            }
            // keep iterating so any other co-located BPs also count their hit
        }
        hit
    }

    /// Same as [`BreakpointSet::should_break`] but evaluates `Breakpoint::condition`
    /// against the supplied register snapshot. A BP without a
    /// condition (or with an empty / whitespace-only condition string)
    /// always passes the condition check and behaves like
    /// `should_break`.
    ///
    /// Condition evaluation rules:
    /// - **true** → the BP triggers (counted, returned as the hit).
    /// - **false** → the BP is silently skipped, *without* consuming
    ///   `ignore_count` and *without* incrementing `hit_count`.
    /// - **parse / runtime error** → fail-safe: trigger the BP. A
    ///   broken condition surfacing as "the program halts here so the
    ///   user notices" is far better than "the program quietly runs
    ///   past the BP because the expression is malformed". The id is
    ///   still returned, so a future UI can highlight the BP and let
    ///   the user fix the condition.
    pub fn check(&mut self, pc: u16, regs: &Registers) -> Option<BreakpointId> {
        let mut hit: Option<BreakpointId> = None;
        for b in self.bps.iter_mut() {
            if !b.enabled || b.addr != pc {
                continue;
            }
            // Evaluate condition (if any) *before* touching ignore /
            // hit counters — a condition that says "no" should make
            // this BP behave as if it weren't here at all for this
            // instruction, including not consuming an ignore tick.
            if let Some(cond) = b.condition.as_deref() {
                let trimmed = cond.trim();
                if !trimmed.is_empty() {
                    match cond::evaluate(trimmed, regs) {
                        Ok(false) => continue,
                        // Parse / runtime errors fall through to
                        // the "trigger" path so the user notices.
                        Ok(true) | Err(_) => {}
                    }
                }
            }
            if b.ignore_count > 0 {
                b.ignore_count -= 1;
                continue;
            }
            b.hit_count = b.hit_count.saturating_add(1);
            if hit.is_none() {
                hit = Some(b.id);
            }
        }
        hit
    }

    pub fn get(&self, id: BreakpointId) -> Option<&Breakpoint> {
        self.bps.iter().find(|b| b.id == id)
    }
}

pub fn dump_registers(cpu: &Cpu) {
    let r = &cpu.r;
    println!(
        "A:{:02X} B:{:02X} X:{:04X} Y:{:04X} U:{:04X} S:{:04X} PC:{:04X} DP:{:02X} CC:{:02X} (E F H I N Z V C) = {}{}{}{}{}{}{}{}",
        r.a, r.b, r.x, r.y, r.u, r.s, r.pc, r.dp, r.cc,
        if (r.cc & 0x80)!=0 {'E'} else {'-'},
        if (r.cc & 0x40)!=0 {'F'} else {'-'},
        if (r.cc & 0x20)!=0 {'H'} else {'-'},
        if (r.cc & 0x10)!=0 {'I'} else {'-'},
        if (r.cc & 0x08)!=0 {'N'} else {'-'},
        if (r.cc & 0x04)!=0 {'Z'} else {'-'},
        if (r.cc & 0x02)!=0 {'V'} else {'-'},
        if (r.cc & 0x01)!=0 {'C'} else {'-'}
    );
}

pub fn dump_memory(mem: &Memory, start: u16, len: usize) {
    let bytes = mem.read_slice(start, len);
    let mut addr = start as usize;
    for chunk in bytes.chunks(16) {
        let addr16 = addr as u16;
        print!("{addr16:04X}:");
        for b in chunk {
            print!(" {b:02X}");
        }
        println!();
        addr += chunk.len();
    }
}

#[allow(clippy::needless_range_loop)]
pub fn dump_memory_bus<B: Bus + ?Sized>(bus: &mut B, start: u16, len: usize) {
    let mut addr = start;
    let mut left = len;
    while left > 0 {
        print!("{addr:04X}:");
        let line = left.min(16);
        for i in 0..line {
            let b = bus.read8(addr.wrapping_add(i as u16));
            print!(" {b:02X}");
        }
        println!();
        addr = addr.wrapping_add(line as u16);
        left -= line;
    }
}

#[allow(clippy::needless_range_loop)]
pub fn dump_memory_ascii<B: Bus + ?Sized>(bus: &mut B, start: u16, len: usize) {
    let mut addr = start;
    let mut left = len;
    while left > 0 {
        let line = left.min(16);
        let mut bytes: [u8; 16] = [0; 16];
        for i in 0..line {
            bytes[i] = bus.read8(addr.wrapping_add(i as u16));
        }
        print!("{addr:04X}:");
        for i in 0..line {
            print!(" {:02X}", bytes[i]);
        }
        for _ in line..16 {
            print!("   ");
        }
        print!("  |");
        for i in 0..line {
            let c = bytes[i];
            let ch = if (0x20..=0x7E).contains(&c) {
                c as char
            } else {
                '.'
            };
            print!("{ch}");
        }
        println!("|");
        addr = addr.wrapping_add(line as u16);
        left -= line;
    }
}
