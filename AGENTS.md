# Repository Guidelines

## Project Structure

* `src/`: emulator core library (`cpu.rs`, `bus.rs`, `mmu.rs`,
  `io.rs`, `disasm.rs`, `debug.rs`, `bootscript.rs`, `loader.rs`,
  `config.rs`, `timer.rs`).
* `tests/`: integration tests, one file per area
  (`cpu_basic.rs`, `console_iobus.rs`, `breakpoints_call_stack.rs`,
  …).  All tests build against the public crate surface only — no
  `#[cfg(...)]` feature gates inside this crate.

## Build / Test / Format

* Build: `cargo build` (Rust 1.70+ recommended).
* Test: `cargo test`
* Format: `cargo fmt --all`
* Lint: `cargo clippy --all-targets`

## Coding Style

* Rust edition 2021, 4-space indent.
* `snake_case` items.  Keep naming consistent with existing modules.
* No GUI / I/O dependencies.  This crate is the **headless** core;
  anything requiring eframe / egui / i18n / serde / image belongs
  in the [em6809](https://github.com/hha0x617/em6809) GUI app.

## Commit & PR Guidelines

* Conventional Commits (`feat:`, `fix:`, `refactor:`, `docs:`,
  `test:`).
* PR-only workflow: no direct pushes to `main`.
* Squash merges preferred.

## Cross-repo coordination

This crate is consumed by:

* [em6809](https://github.com/hha0x617/em6809) — GUI app, source
  of historical context.  Major API changes here typically need a
  matching PR there.
* [emfe_plugins](https://github.com/hha0x617/emfe_plugins) — emfe
  MC6809 plugin, depends on this crate via submodule.

The extraction rationale and migration plan live in
[`docs/extract_em6809_core_plan.md`](https://github.com/hha0x617/em6809/blob/main/docs/extract_em6809_core_plan.md)
within the em6809 repo.
