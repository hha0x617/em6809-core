# Instruction Coverage and Conventions

This document summarizes implemented instructions/modes, disassembly conventions, flags/side effects, and practical verification steps.

> **Note:** verification examples reference sample programs and a
> CLI runner that ship in [em6809](https://github.com/hha0x617/em6809)
> (the GUI host that wraps this crate).  em6809-core itself is a
> library-only crate; pair it with em6809, the
> [emfe MC6809 plugin](https://github.com/hha0x617/emfe_plugins/tree/master/mc6809),
> or your own embedder to actually run the trace samples mentioned
> below.

## Status (2026-04-19)

**The standard MC6809 ISA is 100% implemented.**  All 223 legal opcodes on
the main table plus the full `$10`/`$11` prefix subopcodes are decoded and
executed; the remaining 33 slots of opcode space are ISA-reserved/illegal
and are intentionally left as no-op fallbacks.

Recent completion work (2026-04-19) finished a set of long-standing gaps
and two correctness bugs (LEAS/LEAU swap, indexed PC-relative decode) —
see [Implementation history](#implementation-history) at the bottom.

## Overview

- The emulator covers the entire MC6809 instruction set and all standard
  addressing modes: short/long branches, A/B/D arithmetic, loads/stores for
  X/Y/U/S, memory shifts/rotates/clear/test/jump, stack operations, and
  control ops (MUL, SEX, DAA, SYNC, CWAI, SWI/SWI2/SWI3, ABX, etc.).
- The disassembler spans pages 0x00/0x10/0x11, with unified operand formatting across immediate/direct/indexed/extended and indirect forms; branches show both relative offset and absolute target.

## Disassembly conventions

- Immediate: `#${nn}` (e.g., `#${10}`)
- Direct/Extended: `${addr}` (e.g., `${E000}`)
- Indexed: `±off,REG` (e.g., `-4,X` / `8,U`)
- Indirect: wrap the operand in brackets (e.g., `[10,X]`, `[${E040}]`)
- Branches: `Bcc/LBcc offset -> $${target}`

## Instruction coverage (summary)

- Branches
  - Short `Bcc` set: `BRA`, `BRN`, `BHI`, `BLS`, `BCC`, `BCS`, `BNE`, `BEQ`, `BVC`, `BVS`, `BPL`, `BMI`
  - Long `LBcc` set (pages 0x10/0x11)
  - `BSR` / `LBSR` (samples use LBSR to avoid overflow issues)
- Compare / Load / Store
  - `CMPA/B`, `SUBA/B`, `ADCA/B`, `ADDA/B`, `SBCA/B`, `ANDA/B`, `ORA/B`, `EORA/B`, `BITA/B`
  - `LDA/B/D`, `STA/B/D`, `LDX/U`, `STX/U`, `LDY/S`, `STY/S`, `CPX`, `CPY`, `CMPU`, `CMPS`, `LDS`, `STS`
  - Addressing: immediate/direct/indexed/extended (as defined per instruction)
- Memory ops / Shifts / Rotates / Test
  - `ASL/LSL`, `ASR/LSR`, `ROL`, `ROR`, `TST`, `CLR`, `COM`
  - Targets: direct/indexed/extended; accumulator variants (A/B) where applicable
- Transfer / Control
  - `JMP` (D/X/Y/U/S/extended/indexed/indirect), `JSR` (various modes)
  - `LEAX/Y/U/S`, `PSHS/U`, `PULS/U`, `EXG`, `TFR`
  - `SYNC`, `CWAI`, `MUL`, `DAA`, `SEX`, `ABX`, `ANDCC`, `ORCC`, `SWI/SWI2/SWI3`

## Coverage matrices (operands)

Accumulator (A/B) arithmetic and loads/stores by addressing mode (✓=implemented, —=N/A):

| Group | Instructions | Imm | Direct | Indexed | Extended |
|---|---|:--:|:--:|:--:|:--:|
| 8‑bit arithmetic A/B | `ADDA/B`, `ADCA/B`, `SUBA/B`, `SBCA/B` | ✓ | ✓ | ✓ | ✓ |
| Logic/bit | `ANDA/B`, `ORA/B`, `EORA/B`, `BITA/B` | ✓ | ✓ | ✓ | ✓ |
| Compare A/B | `CMPA/B` | ✓ | ✓ | ✓ | ✓ |
| Load | `LDA/B`, `LDD` | ✓ | ✓ | ✓ | ✓ |
| Store | `STA/B`, `STD` | — | ✓ | ✓ | ✓ |
| Index regs | `LDX/U`, `STX/U` | ✓/— | ✓ | ✓ | ✓ |
| Index regs | `LDY/S`, `STY/S` | ✓/— | ✓ | ✓ | ✓ |
| Compare | `CPX`, `CPY`, `CMPU`, `CMPS` | ✓ | ✓ | ✓ | ✓ |
| Stack ptr | `LDS`, `STS` | ✓/— | ✓ | ✓ | ✓ |

Memory ops and control (—=N/A):

| Group | Instructions | Direct | Indexed | Extended |
|---|---|:--:|:--:|:--:|
| Memory ops | `TST`, `CLR`, `COM` | ✓ | ✓ | ✓ |
| Shifts/rotates | `ASL/LSL`, `ASR/LSR`, `ROL`, `ROR` | ✓ | ✓ | ✓ |
| Jump/JSR | `JMP`, `JSR` | ✓ | ✓ | ✓ |

Branch/page coverage:

| Item | Content | Status |
|---|---|---|
| Short branches | `Bcc` full set | ✓ |
| Long branches | `LBcc` full set (0x10/0x11) | ✓ |
| Subroutines | `BSR`/`LBSR` | ✓ |
| Software interrupts | `SWI`, `SWI2`, `SWI3` | ✓ |

Notes:

- `Imm` stands for immediate. `Indexed` aims to cover variants (5‑bit/8‑bit/16‑bit displacements, indirect).
- For `LDX/U` and `LDY/S`, the Imm column is ✓ only for opcodes that define immediate forms; others are N/A.
- Cycle counts are modeled for instruction cores; platform‑specific memory wait states are not modeled.

## Flags and side effects (policy)

- N/Z/V/C follow the official MC6809 behavior; `DAA/MUL/CWAI` follow spec.
- Compare family (`CMP*`, `CP*`) behaves like subtraction for flags but does not modify data.
- `ANDCC/ORCC` update CCR via bitmasks; `CWAI` updates CCR and handles wait semantics.

## Known conventions/behavior

- Branches show both relative offset and absolute target; traces show post‑resolve PC.
- Indexed indirect uses brackets; composite forms (e.g., D as displacement) follow unified style.

## Verification steps

- Load `samples/traces/trace_all.s19` in the GUI and observe markers/flags with tracing.
- Start PC policy:
  - `--pc` > reset vector > SREC entry (if invalid) > first loaded range start

## Implementation history

### 2026-04-19 (later) — Indexed PC-relative decoding bug

The extended-postbyte (bit7=1) path in `ea_indexed` mis-classified PC
relative mode:

```rust
// Wrong: treated rsel==3 (S) + form=0x04/0x08/0x09/0x0B as PC-relative.
let is_pc_rel = rsel == 3 && matches!(form, 0x04 | 0x08 | 0x09 | 0x0B);
```

On MC6809, PC-relative is encoded solely by form=0x0C (8-bit) / 0x0D
(16-bit); rsel is don't-care for those forms.  Fixed to:

```rust
let is_pc_rel = matches!(form, 0x0C | 0x0D);
```

and added dedicated arms for 0x0C / 0x0D that compute their EA from PC.

Symptom: `lda ,s` (which lwasm encodes as postbyte $E4 when the offset is
zero) read bytes just past the instruction instead of the S stack.  Tiny
Lisp Phase 3 arithmetic primitives (`+`, `-`, `<`) that rely on
`addd ,s` / `cmpx ,s` were all returning garbage.

### 2026-04-19 — ISA coverage completed

Triggered by Tiny Forth (`emfe_plugins/mc6809/examples/forth/forth.asm`)
development, which exposed the remaining gaps and one swapped-opcode bug.

**Bug fix (critical):** `LEAS` ($32) and `LEAU` ($33) were swapped — the
$32 arm updated `U` and the $33 arm updated `S`, the opposite of the ISA.
The CC was also being updated; per ISA these two instructions leave CC
unchanged (only `LEAX`/`LEAY` set Z).  Symptom: `leau 2,u` corrupted the
return stack pointer, eventually leaking a random kernel address onto the
data stack (observed in Tiny Forth as `10 3 - .` printing `1927  ok`).

**Added instructions (33 total):**

- Accumulator: `NEGA` ($40), `NEGB` ($50), `ASRA` ($47), `ASRB` ($57)
- Arithmetic: `SEX` ($1D), `MUL` ($3D), `DAA` ($19)
- `ABX` ($3A)
- Memory R-M-W (dir / idx / ext for each):
  - `NEG` ($00 / $60 / $70)
  - `COM` ($03 / $63 / $73)
  - `LSR` ($04 / $64 / $74)
  - `ROR` ($06 / $66 / $76)
  - `ASR` ($07 / $67 / $77)
  - `ASL/LSL` ($08 / $68 / $78)
  - `ROL` ($09 / $69 / $79)
  - `DEC` ($0A / $6A / $7A)
  - `INC` ($0C / $6C / $7C)
  - `TST` ($0D / $6D / $7D)
- Sync / interrupt: `SYNC` ($13), `CWAI` ($3C)
  - Both approximate waiting behavior as a no-op fall-through; the frame
    push in `CWAI` is performed per spec.

### Remaining "gaps" (reserved / illegal)

All 33 opcode slots below are **reserved or illegal** on the real MC6809
(the immediate-store slots like `$87 STA #` have no meaning, and the rest
are unassigned by Motorola).  They are intentionally unimplemented:

```
$01 $02 $05 $0B $14 $15 $18 $1B $38 $3E
$41 $42 $45 $4B $4E
$51 $52 $55 $5B $5E
$61 $62 $65 $6B
$71 $72 $75 $7B
$87 $8F  $C7 $CD $CF
```
