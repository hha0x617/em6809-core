# em6809-core

[![Build and Test](https://github.com/hha0x617/em6809-core/actions/workflows/ci.yml/badge.svg)](https://github.com/hha0x617/em6809-core/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](LICENSE-APACHE)

*This crate was extracted from [em6809](https://github.com/hha0x617/em6809)
— see [`docs/extract_em6809_core_plan.md`](https://github.com/hha0x617/em6809/blob/main/docs/extract_em6809_core_plan.md)
in that repository for the rationale.  The bulk of the source dates
back to em6809's pre-extraction history, where it was generated with
the assistance of AI coding tools as part of a vibe coding workflow:
through em6809 commit `d85c2eb` with **Codex CLI**, then **Claude Code**
from `aa1759e` onward.  Subsequent changes in this repository continue
under the same workflow with Claude Code.*

Headless emulator core for the **Motorola MC6809**: CPU, bus, MMU
(MC6829), I/O devices (MC6850 console, block storage, GPIO, timer),
disassembler, debugger primitives (breakpoints, shadow call stack,
instruction-boundary tracking), and a trigger-driven boot-script
runtime.

This crate is intentionally GUI-free.  Only two non-`std`
dependencies (`once_cell`, `libc`) are required.

## Status and provenance

`em6809-core` was extracted from
[em6809](https://github.com/hha0x617/em6809) — the GUI emulator
application — so that other embedders can depend on the emulator
core without pulling in eframe / egui / i18n / serde / image /
vte / rfd.  Notable embedders today:

* **[em6809](https://github.com/hha0x617/em6809)** — the GUI
  application that owns the user-facing experience (register
  viewer, listing, memory pane, settings, breakpoints, call
  stack, console window).  Re-exports this crate's modules for
  source-level compatibility.
* **[emfe_plugins](https://github.com/hha0x617/emfe_plugins)** —
  the MC6809 plugin for emfe (Windows-focused emulator framework).

The bulk of the design and history lives in the em6809 repo.  See
[`docs/extract_em6809_core_plan.md`](https://github.com/hha0x617/em6809/blob/main/docs/extract_em6809_core_plan.md)
in em6809 for the extraction rationale, module split rationale,
and migration plan.

## Availability — internal-use crate, not on crates.io

> **`em6809-core` is currently an internal-use crate.**  It is **not
> published on crates.io**, so `cargo add em6809-core` does not work.
> The two known embedders ([em6809](https://github.com/hha0x617/em6809)
> and the [emfe_plugins](https://github.com/hha0x617/emfe_plugins)
> `mc6809` plugin) both depend on this repository directly via Cargo's
> `git`-with-pinned-rev syntax:
>
> ```toml
> em6809-core = { git = "https://github.com/hha0x617/em6809-core", rev = "<commit-sha>" }
> ```
>
> Bumps happen in lockstep across the two consumers so that the
> standalone GUI and the plugin DLL run identical CPU-core code.
>
> The intent is to publish `em6809-core` on crates.io and switch both
> consumers to a normal `version = "..."` pin once the public API
> stabilises — until then, treat this crate as an implementation detail
> shared between the two repositories above.

## Crate layout

| Module        | What it is |
|---------------|------------|
| `cpu`         | MC6809 CPU implementation, registers, instruction step. |
| `bus`         | `Bus` trait, flat `Memory`, and the `WriteTrack` wrapper used for self-modifying-code re-disassembly. |
| `mmu`         | MC6829 paging MMU (used by NetBSD on MVME147). |
| `io`          | `IoBus`, MC6850-compatible console (`Mc6850Dev`), block device, GPIO, timer wiring. |
| `loader`      | S-Record / BIN parsers. |
| `disasm`      | Instruction-level disassembler. |
| `debug`       | Breakpoints, shadow call stack, instruction-boundary tracker, conditional-breakpoint expression evaluator. |
| `bootscript`  | The `.boot` DSL parser and `BootSequencer` runtime — drives the `--boot-script` flow in em6809 and is reusable by embedders. |
| `config`      | Cross-cutting configuration types. |
| `timer`       | Periodic timer device. |

The integration tests in `tests/` mirror the modules above; they
build only against this crate (no GUI feature gates) and act as
the public API regression net.

## Usage

Add the crate as a git dependency until we publish to crates.io:

```toml
[dependencies]
em6809-core = { git = "https://github.com/hha0x617/em6809-core", rev = "<commit-sha>" }
```

Pin the `rev` to a specific commit while the API is still in flux.

Minimal usage:

```rust
use em6809_core::bus::Memory;
use em6809_core::cpu::Cpu;
use em6809_core::loader;

fn run_image(srec: &str) {
    let mut bus = Memory::new();
    let _img = loader::parse_srec(srec).expect("valid S-Record");
    // ... copy `img` ranges into `bus`, set PC, then step the CPU
    let mut cpu = Cpu::new();
    cpu.r.pc = 0x0100;
    let _stop = cpu.step(&mut bus);
}
```

See the integration tests in `tests/` for fuller examples
(`cpu_basic.rs`, `console_iobus.rs`, `breakpoints_call_stack.rs`,
`page2_index_ops.rs`, `swi_push_order.rs`, etc.).

## Documentation

In-tree docs (under `docs/`):

| Topic                       | English (canonical)                | 日本語                            |
|-----------------------------|------------------------------------|-----------------------------------|
| MC6809 instruction coverage | `docs/en/instruction_status.md`    | `docs/ja/instruction_status.md`   |
| OS‑9 device map templates   | `docs/en/os9_device_map_templates.md` | `docs/ja/os9_device_map_templates.md` |

The full index lives in [`docs/README.md`](docs/README.md), which
also points at the related em6809 docs (extraction plan, GUI stack
notes, OS-9 boot guide, config-vs-boot-script interaction).

## Building

* Stable Rust toolchain (`rustup default stable`).
* `cargo build`
* `cargo test`

The crate builds cleanly on Linux, macOS, and Windows without any
platform-specific configuration.

## Versioning

While we settle the API surface we stay on `0.x` and consumers pin
to a commit SHA.  Any breaking change to a `pub` item bumps the
minor; internal cleanups bump the patch.  Once the boundary
stabilises we move to crates.io.

## License

Dual-licensed under MIT OR Apache-2.0.  See `LICENSE-MIT` and
`LICENSE-APACHE` for the full texts.

## Contributing

PRs welcome.  Per the project policy, the canonical branch is
`main` and changes ship via PR (no direct pushes).  Keep changes
mechanical and surgical — the design rationale lives in the em6809
repo's `docs/`.
