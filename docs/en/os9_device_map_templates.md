# OS-9 Device Map Templates (MC6809)

This document provides suggested I/O mappings and interrupt vector notes for OS‑9–style setups on MC6809. Adjust addresses to your target image and board configuration.

> **Note:** the runnable command examples below
> (`cargo run -- samples/...`) are for [em6809](https://github.com/hha0x617/em6809),
> the GUI host that wraps this crate and ships the trace and timer
> samples referenced.  em6809-core itself is a library-only crate;
> the I/O page / vector / timer guidance still applies to any
> embedder, including the
> [emfe MC6809 plugin](https://github.com/hha0x617/emfe_plugins/tree/master/mc6809).

## I/O Page Suggestions
- Console (UART/ACIA): base `0xFF00`
  - Control/Status: `0xFF00`
  - Data RX/TX:     `0xFF01`
  - Optional aux:   `0xFF02..0xFF03`
- Timer: `0xFF10`
- Disk/Block I/O: `0xFF20`
- Free/Reserved: `0xFF30..0xFFEF`

Notes:
- Keep the I/O page contiguous at `0xFF00..0xFFFF` for simplicity.
- Use the boot script to set console IRQ/FIRQ behavior and watermarks.

## Interrupt Vectors (Reference)
MC6809 vectors reside in the top of memory. Typical usage (adjust per ROM):
- FIRQ: fast IRQ handler (e.g., high‑rate console RX)
- IRQ: standard interrupt handler (e.g., timer)
- NMI: non‑maskable for critical faults
- RESET: system entry

Recommendations:
- Write‑protect vector page after initialization (e.g., via `attr`/`prot` in the boot script).
- Route console RX to FIRQ when low latency is preferred; otherwise use IRQ.

## Boot Script Hints
- Use `on_step`/`on_pc` to apply MMU maps and enable device IRQs in phases.
- Apply `con_ctrl`, `con_rx_wm`, and optional `con_irq_hold` to tune console behavior.
- Switch from `mode system` to `mode user` only after kernel pages are mapped.

## Timer (0xFF10) Quick Reference
- Base: `0xFF10`
- Registers (offsets from base):
  - `+0` CTRL/STATUS (R/W): bit0 RUN, bit1 IRQ_EN, bit2 FIRQ, bit3 PENDING (R), bit4 write-1-to-clear PENDING
  - `+1..+2` RELOAD (R/W): 16-bit period in instruction ticks (big-endian)
  - `+3..+4` COUNTER (R/W): current down-counter
- IRQ routing: when `FIRQ` bit is set, assertions appear on FIRQ; otherwise on IRQ.

Example CLI usage (no OS-9 kernel required):
- Use the built-in `trace_all.s19` or any tight loop image, then attach a timer:
  - `cargo run -- samples/traces/trace_all.s19 --timer 0xFF10 --timer-reload 0x0010 --timer-irq --timer-start`
  - Or derive reload from rate: `--timer-rate 1000 --timer-ips 1000000`

LED blink sample (~1 Hz):
- Assemble and run `samples/traces/timer_led.asm` which toggles GPIO bit0 via the timer ISR.
- Recommended command (FIRQ route, 60 Hz ticks → 1 blink/sec):
  - `cargo run -- samples/traces/timer_led.s19 --gpio 0xFF30 --gpio-bits 8 --timer 0xFF10 --timer-rate 60 --timer-ips 1000000 --timer-irq --timer-firq --timer-start --run`

Validation tests:
- See `tests/timer.rs` for unit tests that exercise IRQ/FIRQ routing and pending-bit clearing at `$FF10`.
