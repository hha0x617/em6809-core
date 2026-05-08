# Documentation Index

Conventions follow [em6809](https://github.com/hha0x617/em6809):
**English is canonical** under `en/`, with Japanese companion
translations under `ja/` aiming for content parity.

## Topics

| Topic                                | English (canonical)             | 日本語 (companion)             |
| ------------------------------------ | ------------------------------- | ------------------------------ |
| MC6809 instruction coverage          | `en/instruction_status.md`      | `ja/instruction_status.md`     |
| OS‑9 device map templates            | `en/os9_device_map_templates.md`| `ja/os9_device_map_templates.md` |

### One-line summaries

- **Instruction coverage** — current MC6809 opcode coverage (the
  standard ISA is 100% implemented), disassembly conventions, flag
  semantics, and how to verify behaviour against the trace samples.
- **OS‑9 device map templates** — recommended I/O page layout,
  interrupt vector hints, and the timer-at-`$FF10` quick reference.
  The architectural guidance applies to any embedder; the runnable
  command examples target [em6809](https://github.com/hha0x617/em6809),
  the GUI host that ships the trace samples.

## Related documentation in em6809

These docs live in [hha0x617/em6809](https://github.com/hha0x617/em6809)
because they describe the GUI host, not the library itself:

- [`docs/extract_em6809_core_plan.md`](https://github.com/hha0x617/em6809/blob/main/docs/extract_em6809_core_plan.md)
  — the rationale and phased plan for the em6809 → em6809-core
  extraction.  Read this first if you want context on how the two
  crates relate.
- [`docs/en/config_and_boot_script.md`](https://github.com/hha0x617/em6809/blob/main/docs/en/config_and_boot_script.md)
  — how `config.toml` / CLI flags interact with boot scripts at
  runtime (Phase 1 / 2 / 3 sequence, interaction matrix, footguns).
  Boot-script execution lives in this crate
  ([`bootscript`](../src/bootscript.rs)), but the document's frame
  of reference is em6809's settings/CLI surface.
- [`docs/en/gui_stack.md`](https://github.com/hha0x617/em6809/blob/main/docs/en/gui_stack.md)
  — eframe / egui stack notes for the GUI app (irrelevant to
  embedders).
- [`docs/en/os9_guide.md`](https://github.com/hha0x617/em6809/blob/main/docs/en/os9_guide.md)
  — current OS-9 / NitrOS-9 boot status (not yet bootable; see
  the roadmap).
