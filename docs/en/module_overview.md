# Module overview

Each module's `cargo doc` page (its top-of-file `//!` block) is the
**canonical** reference; the entries below are GitHub-readable
mirrors for orientation and quick scanning without cloning the repo
locally.

## `bus` — address-bus abstraction

Defines the `Bus` trait that the CPU reaches the world through, and
ships two ready-made implementations.  Embedders that need a
peripheral-aware bus build on top of these (see `io::IoBus` in the
`io` module).

- `trait Bus { fn read8(&mut self, u16) -> u8; fn write8(...); ... }`
  — extension points include `read8_fetch` (execute-permission split
  from data reads) and `irq_lines() -> (irq, firq, nmi)`.
- `Memory` — flat 64 KiB `[u8; 0x10000]`.  Helpers: `clear(value)`,
  `load_slice(base, &[u8])`, `read_slice(start, len) -> &[u8]`.
- `WriteTrack` — wraps any `Box<dyn Bus>` and records every write
  inside an optional address span.  Used by em6809 to re-disassemble
  self-modifying code regions.  Helpers: `set_span(...)`,
  `take_dirty_addrs() -> Vec<u16>`, `inner_any_mut()`.

```rust
use em6809_core::bus::{Bus, Memory};

let mut bus = Memory::new();
bus.load_slice(0x0100, &[0x12, 0x12, 0x39]); // NOP NOP RTS
assert_eq!(bus.read8(0x0102), 0x39);
```

## `cpu` — MC6809 CPU and registers

The module embedders touch most often.  Owns CPU state and the
per-instruction step routine.

- `Registers` — `a`, `b`, `x`, `y`, `u`, `s`, `pc`, `dp`, `cc`.
  `Copy + Clone + Default`, snapshottable by value.
- `Cpu` — `cpu.r: Registers`, `cpu.cycles: u64`, embedded
  `debug::ShadowCallStack`, plus `nmi_pending` / `firq_pending` /
  `irq_pending` latches.  Methods:
  - `Cpu::new()` — fresh, all-zero CPU.
  - `Cpu::reset(&mut bus)` — load PC from reset vector at `$FFFE/F`.
  - `Cpu::set_pc(u16)` — start anywhere (used by the `--pc` CLI flag).
  - `Cpu::step(&mut bus, trace) -> u32` — one instruction; returns
    cycles consumed.
  - `Cpu::step_over(...)` / `Cpu::step_out(...)` — debugger
    primitives, return a `StepStop` reason.
  - `Cpu::request_nmi()` / `request_firq()` / `request_irq()` —
    latch a pending interrupt; serviced on the next `step()`.
- `enum StepStop` — `ReturnTarget`, `Breakpoint(BreakpointId)`,
  `Limit`, `NotACall`, `EmptyStack`.
- Free functions: `set_irq_log(bool)` (global IRQ trace toggle),
  `regs_snapshot(&Cpu) -> Registers` (UI-friendly clone).

```rust
use em6809_core::bus::Memory;
use em6809_core::cpu::Cpu;

let mut bus = Memory::new();
let mut cpu = Cpu::new();
cpu.reset(&mut bus);              // PC <- vector at $FFFE/F
let cycles = cpu.step(&mut bus, /* trace = */ false);
println!("first instruction took {cycles} cycles");
```

## `loader` — image parsers and bus loaders

Reads program images and either returns a structured parse result
or writes them straight into a bus / memory.

- `enum ImageFormat { Binary, Srec }` — pairs with the GUI's
  `--format` flag.
- `struct ParsedImage { blocks: Vec<(u16, Vec<u8>)>, loaded_ranges,
  entry: Option<u16> }` — full parse result.
- `struct LoadedImage { loaded_ranges, entry: Option<u16> }` —
  post-load summary, no byte payload.
- `parse_binary(base, &[u8]) -> ParsedImage` — single-block wrap.
- `parse_srec(&str) -> Result<ParsedImage, String>` — Motorola
  S-Record parser; accepts S0/S1/S2/S3/S7/S8/S9 records.
- `load_binary(...)` / `load_binary_bus(...)` / `load_srec(...)` /
  `load_srec_bus(...)` — `parse_*` then write into a `Memory` or any
  `Bus`.

```rust
use em6809_core::bus::Memory;
use em6809_core::loader::load_srec;

let srec = std::fs::read_to_string("hello.s19").unwrap();
let mut mem = Memory::new();
let img = load_srec(&mut mem, &srec).expect("valid S-Record");
if let Some(entry) = img.entry {
    println!("entry point: ${:04X}", entry);
}
```

For loaders that need to honour peripheral writes (so writes to ACIA
/ GPIO / etc. don't get clobbered), use the `_bus` variants on top of
`io::IoBus`.

## `disasm` — single-instruction and window disassembler

Reads bytes from any `Bus` and turns them into mnemonic text.  The
em6809 GUI uses this to render the listing pane; integration tests
use it as the canonical "this PC decoded as that instruction"
verification.

- `disasm_one(bus, pc) -> (u16, String)` — `(byte_length, "MNEMONIC
  OPERAND")`.  Advance `pc` by `byte_length` for the next
  instruction.
- `disasm_one_hex(bus, pc) -> (u16, String)` — same, but the string
  is prefixed with raw bytes (`"$1F $89 ..."`) for hex-dump views.
- `disasm_window(bus, pc, before, after) -> Vec<DisasmLine>` — band
  of instructions around `pc`, anchored on a known instruction
  boundary.  Robust against landing mid-instruction (common when
  scrolling).
- `type DisasmLine = (u16, String)` — `(address, mnemonic_text)`.

```rust
use em6809_core::bus::Memory;
use em6809_core::disasm::disasm_one;

let mut bus = Memory::new();
bus.load_slice(0x0100, &[0x12]);  // NOP
let (len, text) = disasm_one(&mut bus, 0x0100);
assert_eq!((len, text.as_str()), (1, "NOP"));
```

## `io` — peripherals and the device-aware bus

Wraps a plain `Bus` (or MMU-backed bus) with a list of memory-mapped
devices, so reads/writes to peripheral address ranges are handled by
the device rather than RAM.  This is the actual CPU bus that
em6809 / emfe_plugin_mc6809 hand to the CPU.

- `trait Device` — every peripheral implements `contains(addr)`,
  `read8/write8`, plus optional `irq_lines() -> (irq, firq, nmi)`.
- `Mc6850Dev` — Motorola **MC6850 ACIA**-compatible UART (`+0` SR/CR,
  `+1` RDR/TDR).  Helpers: `feed_bytes(&[u8])`, output tee
  (`set_out_file`, `set_tee_stderr`, `set_flush_*`, `set_local_echo`),
  IRQ/FIRQ wiring (`set_irq_hold_cycles`, `set_firq`).  Used by Hha
  Forth, Hha Lisp, the NetBSD MVME147 boot ROM.
- `BlockDev` — sector-addressable disk backed by a host file
  (`set_backing_file`) or in-memory image (`set_image`).  Exposes
  `last_cmd()`, `last_data()`, `status()`, `take_dirty()`.
- `GpioDev` — generic memory-mapped GPIO (`get_state() -> (out, dir,
  value)`).
- `IoBus<B>` — the device-aware bus itself.  Holds `inner: B` plus a
  `Vec<Box<dyn Device>>`.
  - `IoBus::new(inner)` / `add_device(dev)`.
  - `ensure_console` / `ensure_block` / `ensure_gpio` / `ensure_timer`
    — install or remove the standard peripherals from a config flag.
  - `with_console_mut` / `with_block_mut` / `with_gpio_mut` /
    `with_timer_mut` — run a closure with mutable access to the
    device.
  - `feed_console_input(bytes)` — shorthand for the common case.
- Free functions wire the device to the GUI: `set_console_log`,
  `set_console_gui_*`, `take_console_gui_bytes`,
  `set_console_repaint_callback`, `publish_gpio_broadcast`,
  `take_gpio_broadcast`, `peek_gpio_broadcast`.

```rust
use em6809_core::bus::Memory;
use em6809_core::cpu::Cpu;
use em6809_core::io::IoBus;

// Plain Memory + console at $FF00 + a small block disk at $FF10.
let mut bus = IoBus::new(Memory::new());
bus.ensure_console(true, 0xFF00);
bus.ensure_block(true, 0xFF10);

let mut cpu = Cpu::new();
cpu.reset(&mut bus);
for _ in 0..1_000 {
    cpu.step(&mut bus, /* trace = */ false);
}
```

## `mmu` — MC6829 paging MMU

The Motorola **MC6829** paging MMU.  Used by NetBSD on the MVME147
platform.  16 logical pages × 4 KiB = 64 KiB CPU address space, with
each page mappable to a 16-bit physical frame; up to 8 task contexts;
per-page write/read/execute attributes; configurable register window.

- `Mc6829` — implements `Bus` so it plugs in anywhere a bus is
  expected.  Methods:
  - `Mc6829::new(phys_bytes, regs_base)` — fresh MMU.  `regs_base` is
    the logical address of the configuration window.
  - `identity_map_current()` — bypass: logical N → physical N for the
    current task.  Default boot state until the OS programs the map.
  - `set_task(t: u8)`, `set_map_entry(page, frame)` — directly poke
    the active task's map.
  - `snapshot_current_map()`, `snapshot_map_for(sys_mode)`,
    `snapshot_maps()` — read the active map (or both user/system
    maps) for UI/debugger display.
  - `store_logical_slice(base, &[u8])` /
    `store_physical_slice(pbase, &[u8])` /
    `clear_physical(value)` — load image data at logical or physical
    addresses.
  - `set_log_maps(bool)` — verbose translation logging.

Configuration via DSL (`task N`, `map page=frame`, `attr ...`,
`prot ...`) lives in `config`.  Boot-time programming via triggers
(`OnPc`, `OnStep`) lives in `bootscript`.

```rust
use em6809_core::cpu::Cpu;
use em6809_core::mmu::Mc6829;

// 64 KiB physical, register window at $FFE0 (logical).
let mut mmu = Mc6829::new(0x10000, 0xFFE0);
mmu.identity_map_current();
mmu.store_logical_slice(0x0100, &[0x12, 0x12, 0x39]); // NOP NOP RTS

let mut cpu = Cpu::new();
cpu.set_pc(0x0100);
cpu.step(&mut mmu, /* trace = */ false);
```

## `timer` — minimal periodic timer device

A small memory-mapped countdown timer that drives periodic interrupts.
Implements `Device`, so plugs into `IoBus`.

- Register layout: `+0` CTRL/STATUS (`RUN`, `IRQ_EN`, `FIRQ`, `PENDING`),
  `+1..+2` `RELOAD` (16-bit period in instruction ticks), `+3..+4`
  `COUNTER`.
- `TimerDev` — methods:
  - `TimerDev::new(base)` — fresh, stopped.
  - `set_reload(u16)` / `start()` / `stop()` — programmatic control
    without going through register writes (testing).
  - `set_irq_enable(bool)` / `set_firq(bool)` — IRQ/FIRQ wiring
    without touching CTRL.
  - `get_state() -> (run, irq_en, firq, pending)` — UI snapshot.
  - `get_info() -> (reload, counter)` — UI snapshot.

```rust
use em6809_core::bus::Memory;
use em6809_core::io::IoBus;
use em6809_core::timer::TimerDev;

let mut bus = IoBus::new(Memory::new());
let mut t = TimerDev::new(0xFF20);
t.set_reload(10_000);
t.set_irq_enable(true);
t.start();
bus.add_device(t);
```

Or, simpler, let `IoBus::ensure_timer` handle install/teardown from a
config flag.

## `debug` — debugger primitives

Everything the GUI's debugger surface needs that isn't on the CPU
itself: breakpoints (with conditional expression evaluator), shadow
call stack, instruction-boundary tracking, and a small set of
memory/register dump helpers.  The `Cpu` embeds a `ShadowCallStack`
and consults a `BreakpointSet` every step, so embedders typically
just configure these and let the CPU do the bookkeeping.

**Breakpoints**

- `BreakpointId` — opaque newtype around `u32`.  Returned by
  `BreakpointSet::add()` and used to identify breakpoints later.
- `Breakpoint` — `pub address: u16`, `pub enabled: bool`,
  `pub condition: Option<String>`, `pub hit_count: u64`,
  `pub ignore_count: u64`.
- `BreakpointSet` — owns the `Vec<Breakpoint>`.  Methods:
  - `add(addr) -> BreakpointId` / `remove(id)` /
    `set_enabled(id, bool)` / `set_condition(id, Option<String>)`.
  - `should_break(pc)` — fast pre-step check.
  - `check(pc, &Registers)` — full check including condition
    evaluation; called when `should_break` returned `Some`.
  - `iter()` / `len()` for UI rendering.
  - The condition expression language supports `==`, `!=`, `<`,
    `<=`, `>`, `>=`, `&&`, `||`, `!`, `+`, `-`, `*`, `&`, `|`,
    `^`, parentheses, hex (`0x..` / `$..`), decimal, register names
    (`a`/`b`/`d`/`x`/`y`/`u`/`s`/`pc`/`dp`/`cc`).

**Shadow call stack**

- `CallKind` — what pushed this frame (`Bsr` / `Lbsr` / `Jsr` / `Swi`
  / `Irq` / `Firq` / `Nmi`).
- `CallFrame` — `pub return_addr: u16`, `pub kind: CallKind`, plus a
  few register snapshots for UI display.
- `ShadowCallStack` — append-only `Vec<CallFrame>`.  `frames()`,
  `top()`, `depth()` for read access; the CPU pushes/pops directly.

**Instruction boundaries**

- `InstructionBoundaries` — set of address ranges known to start
  instruction boundaries.  Lets the GUI's listing pane scroll
  without landing in the middle of a multi-byte op.
- `linear_sweep(...)` — populate an `InstructionBoundaries` from a
  linear walk of an address range.

**Free dump helpers**

- `dump_registers(&cpu)` — print to stdout (CLI / test usage).
- `dump_memory(&mem, start, len)` / `dump_memory_bus(&mut bus, ...)`
  / `dump_memory_ascii(&mut bus, ...)` — hex / hex+ASCII dumps for a
  `&Memory` or any `&mut Bus`.

```rust
use em6809_core::bus::Memory;
use em6809_core::cpu::Cpu;
use em6809_core::debug::BreakpointSet;

let mut bus = Memory::new();
let mut cpu = Cpu::new();
let mut bps = BreakpointSet::default();
let id = bps.add(0x1234);
bps.set_condition(id, Some("a == 0x42 && pc < $2000".into()));

cpu.reset(&mut bus);
loop {
    if let Some(hit) = bps.check(cpu.r.pc, &cpu.r) {
        println!("stopped on bp {:?}", hit);
        break;
    }
    cpu.step(&mut bus, false);
}
```

## `config` — MMU configuration DSL

A small text-based configuration language for programming `Mc6829`
state up front.  Used by the em6809 GUI's `--mmu-config <file>` flag
and by integration tests that want a non-trivial MMU without manually
poking each register.

**Syntax** — whitespace-separated tokens; one statement per line.
`#` and `//` start line comments.  Numbers accept hex (`0x..` /
`$..`) or decimal.

| Statement | Effect |
|---|---|
| `task N` | Switch the active task to `N` (0..7). |
| `mode sys` / `mode user` | Switch CPU mode bit. |
| `map P=F` | Map logical page `P` to physical frame `F`. |
| `attr P=B` | Set attribute byte for logical page `P` (bit0 WPROT, bit1 RPROT, bit2 NX). |
| `prot N` | Write protection-control byte. |
| `regs <addr>` / `regs off` | Move or disable the register window. |

**Provided functions**

- `apply_mmu_config_from_str(&mut Mc6829, &str) -> Result<(), String>`
  — parse + apply.  Returns `Err(line N: ...)` on any parse error so
  the caller can surface the location to the user.
- `apply_preset(&mut Mc6829, &str) -> Result<(), String>` — apply a
  named built-in preset (`identity`, `netbsd_mvme147`, etc.).  Useful
  when you don't want to author a config file just to get a
  known-good starting point.
- `list_presets() -> &'static [&'static str]` — preset names.  Drives
  the GUI's preset dropdown.

```rust
use em6809_core::mmu::Mc6829;
use em6809_core::config::{apply_mmu_config_from_str, apply_preset};

let mut mmu = Mc6829::new(0x10000, 0xFFE0);
apply_preset(&mut mmu, "identity").unwrap();

let cfg = "
    task 0
    map 0x0=0x0000  // logical $0xxx → physical frame 0
    map 0x1=0x0001
    attr 0xF=0x01   // WPROT on $Fxxx
";
apply_mmu_config_from_str(&mut mmu, cfg).unwrap();
```

## `bootscript` — trigger-driven boot-script DSL

A small DSL for "do X when Y happens" during emulation.  Used by
em6809's `--boot-script` CLI option to set up MMU mappings, console /
block / timer device state, and CPU interrupt-mask state at specific
PCs or step counts during the boot sequence.  Reusable by any
embedder that wants the same DSL.

**Syntax** — each line is `<trigger>: <action>`.  Triggers:

- `at_pc <addr>` — fire when `cpu.r.pc == addr`.
- `at_step <N>` — fire when the global instruction count reaches
  `N`.

Actions cover the common boot-time configuration knobs — see the
`Action` enum for the full list.  Comments start with `#` or `//`;
blank lines are ignored.

**Provided types and functions**

- `enum Action` — `Mode(bool)`, `Prot(u8)`, `Map(usize, u16)`,
  `Attr(usize, u8)`, `ConCtrl(u8)`, `ConRxWm(usize)`,
  `ConIrqHold(u32)`, `ConFirq(bool)`, `IrqMask(bool)`, `FirqMask(bool)`,
  `BlkIrq(bool)`, `BlkFirq(bool)`, `BlkIrqHold(u32)`.
- `enum Trigger` — `OnPc(addr, Action)` or `OnStep(n, Action)`.
- `BootSequencer` — owns a `Vec<Trigger>` and a "next step counter".
  Constructed with `BootSequencer::new(triggers)`.  The CPU loop
  calls `seq.on_pre_step(&mut bus, regs_base, pc, &mut cpu)` and
  `seq.on_post_step(&mut bus, regs_base)` to fire matching `OnPc` /
  `OnStep` triggers in order.  Each trigger fires at most once.
  Diagnostic getters `console_missing_count()`,
  `block_missing_count()`, `mmu_missing_count()` count silent no-ops
  when an action targeted a device the bus doesn't have.
- `parse_boot_script(&str) -> Result<Vec<Trigger>, String>` — parser.
  `Err(line N: ...)` on parse failure.
- `emit_boot_template(name: &str) -> String` — return a sample boot
  script for a named scenario (`netbsd_mvme147` and other presets).

```rust
use em6809_core::bus::Memory;
use em6809_core::cpu::Cpu;
use em6809_core::io::IoBus;
use em6809_core::bootscript::{BootSequencer, parse_boot_script};

let script = "
    at_pc $0100: mode sys
    at_pc $0100: map 0=0
    at_step 1000: con_ctrl 0x55
";
let triggers = parse_boot_script(script).expect("valid script");
let mut seq = BootSequencer::new(triggers);

let mut bus = IoBus::new(Memory::new());
let mut cpu = Cpu::new();
cpu.reset(&mut bus);
let regs_base = 0xFFE0;
loop {
    seq.on_pre_step(&mut bus, regs_base, cpu.r.pc, &mut cpu);
    cpu.step(&mut bus, false);
    seq.on_post_step(&mut bus, regs_base);
}
```

See em6809's
[`docs/en/config_and_boot_script.md`](https://github.com/hha0x617/em6809/blob/main/docs/en/config_and_boot_script.md)
for the full script grammar and known footguns (config-vs-script
ordering, silent no-op when target devices are absent, MMU base
validation).
