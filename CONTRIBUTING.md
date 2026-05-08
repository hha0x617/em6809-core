# Contributing to em6809-core

Thanks for your interest!  em6809-core is the headless emulator core
for the **Motorola MC6809** — CPU, bus, MMU (MC6829), I/O devices,
disassembler, debugger primitives, and a trigger-driven boot-script
runtime — extracted from
[em6809](https://github.com/hha0x617/em6809) so other embedders
(notably the [emfe MC6809 plugin](https://github.com/hha0x617/emfe_plugins/tree/master/mc6809))
can depend on it without the GUI dependency tree.

## Getting the source

```bash
git clone https://github.com/hha0x617/em6809-core.git
```

## Build prerequisites

- **Rust stable** ([install via rustup](https://rustup.rs))

That's it.  This crate has only two non-`std` dependencies
(`once_cell`, `libc`) and **no platform-specific configuration** —
it builds cleanly on Linux, macOS, and Windows out of the box.

## Building and testing

```bash
# Build
cargo build

# Run the test suite (~93 cases across 12 binaries)
cargo test

# Style / lint
cargo fmt --all -- --check
cargo clippy --all-targets
```

## Cross-repo coordination

This crate is consumed by:

- [em6809](https://github.com/hha0x617/em6809) — the GUI app
  re-exports the modules here for source-level compatibility.
- [emfe_plugins](https://github.com/hha0x617/emfe_plugins) — the
  MC6809 plugin pulls this crate via submodule.

Major API changes here usually need a matching PR on the consumer
side.  Keep changes mechanical and surgical when you can; the
broader design rationale lives in
[`em6809/docs/extract_em6809_core_plan.md`](https://github.com/hha0x617/em6809/blob/main/docs/extract_em6809_core_plan.md).

## Making a change

1. Fork the repository and create a feature branch off `main`.
2. Keep commits small and focused.  CPU correctness fixes deserve
   their own commits with a reproducer in the message body.
3. Run `cargo fmt` + `cargo test` before pushing.
4. Open a pull request against `main`.  CI must pass before merge.

## Commit style

- Subject line ≤ 72 chars, imperative mood, optional `type(scope):`
  prefix (`feat(cpu):`, `fix(mmu):`, `docs:`, `ci:`, `chore:`).
- Body wrapped to 72 chars, focused on motivation and trade-offs.
- For ISA correctness fixes, include the failing case (mnemonic,
  operands, expected vs observed register / memory state) so
  reviewers can reproduce it quickly.

## Reporting bugs / requesting features

Use the issue templates in
[`.github/ISSUE_TEMPLATE/`](.github/ISSUE_TEMPLATE/).  Security
vulnerabilities go through [`SECURITY.md`](SECURITY.md) instead.

## License

By submitting a contribution you agree it will be licensed under the
same dual **MIT OR Apache-2.0** terms as the rest of the repository,
without any additional terms or conditions.
