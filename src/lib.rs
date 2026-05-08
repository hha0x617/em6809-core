//! em6809-core
//!
//! Headless emulator core for the Motorola MC6809.  This crate
//! contains everything the CPU and its bus need to run, with no
//! GUI, no rendering, no localisation, and only two non-`std`
//! dependencies (`once_cell`, `libc`).
//!
//! It was extracted from the [em6809](https://github.com/hha0x617/em6809)
//! GUI application so that other embedders (notably the
//! [emfe_plugins](https://github.com/hha0x617/emfe_plugins) MC6809
//! plugin) can depend on the emulator core without pulling in
//! eframe / egui / i18n / serde / image / vte / rfd.
//!
//! See the per-module docs for entry points.  The most common
//! starting points are:
//!
//! * [`cpu::Cpu`] — the MC6809 CPU itself.
//! * [`bus::Bus`] / [`bus::Memory`] — the bus trait and a flat
//!   64 KiB memory implementation.
//! * [`io::IoBus`] — a wrapping bus that adds MC6850 console,
//!   block, GPIO, and timer devices.
//! * [`mmu::Mc6829`] — the MC6829 MMU (used by NetBSD on the
//!   MVME147 platform, among others).
//! * [`bootscript::BootSequencer`] — the trigger-driven boot
//!   script runtime used by em6809's `--boot-script` CLI option,
//!   reusable by any embedder that wants the same DSL.

pub mod bootscript;
pub mod bus;
pub mod config;
pub mod cpu;
pub mod debug;
pub mod disasm;
pub mod io;
pub mod loader;
pub mod mmu;
pub mod timer;
