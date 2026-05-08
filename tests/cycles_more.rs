use em6809_core::bus::Memory;
use em6809_core::cpu::Cpu;

#[test]
fn bne_taken_not_taken_cycles() {
    // Not taken: LDA #$00 (Z=1); BNE +2 (not taken)
    let mut mem = Memory::new();
    mem.load_slice(0x0000, &[0x86, 0x00, 0x26, 0x02]);
    let mut cpu = Cpu::new();
    cpu.set_pc(0x0000);
    cpu.r.s = 0x0200; // avoid clobbering vectors
    cpu.step(&mut mem, false); // LDA #0
    let c0 = cpu.cycles;
    let pc0 = cpu.r.pc;
    cpu.step(&mut mem, false); // BNE not taken
    assert_eq!(cpu.cycles - c0, 2, "BNE not taken should cost 2");
    assert_eq!(cpu.r.pc, pc0 + 2, "PC advances when not taken");

    // Taken: LDA #$01 (Z=0); BNE -2 (taken)
    let mut mem = Memory::new();
    mem.load_slice(0x0100, &[0x86, 0x01, 0x26, 0xFE]);
    let mut cpu = Cpu::new();
    cpu.set_pc(0x0100);
    cpu.step(&mut mem, false); // LDA #1
    let c1 = cpu.cycles;
    cpu.step(&mut mem, false); // BNE taken
    assert_eq!(cpu.cycles - c1, 3, "BNE taken should cost 3");
    // After fetch PC=0x0104; add -2 => 0x0102
    assert_eq!(cpu.r.pc, 0x0102);
}

#[test]
fn lda_indexed_abs_indirect_cycles() {
    // LDA [n16] where [0x2000] = 0x1005, mem[0x1005]=0x77
    let mut mem = Memory::new();
    mem.load_slice(0x0000, &[0xA6, 0x9F, 0x20, 0x00]);
    mem.load_slice(0x2000, &[0x10, 0x05]);
    mem.load_slice(0x1005, &[0x77]);
    let mut cpu = Cpu::new();
    cpu.set_pc(0x0000);
    let c0 = cpu.cycles;
    cpu.step(&mut mem, false);
    assert_eq!(cpu.r.a, 0x77);
    assert_eq!(cpu.cycles - c0, 9, "4 base + 5 [n16] indirect");
}

#[test]
fn pshs_puls_cycles_and_data() {
    // PSHS with mask CC|A|B|DP = 0x0F (4 bytes), then PULS same mask
    let mut mem = Memory::new();
    let mut cpu = Cpu::new();
    cpu.set_pc(0x0000);
    cpu.r.s = 0x0100;
    cpu.r.cc = 0xAA;
    cpu.r.a = 0x12;
    cpu.r.b = 0x34;
    cpu.r.dp = 0x5E;
    mem.load_slice(0x0000, &[0x34, 0x0F, 0x35, 0x0F]);
    cpu.step(&mut mem, false); // PSHS
    assert_eq!(cpu.cycles, 5 + 4);
    assert_eq!(cpu.r.s, 0x0100 - 4);
    let s = cpu.r.s;
    // Push order in implementation: DP, B, A, CC
    assert_eq!(mem.read_slice(s, 4), &[0xAA, 0x12, 0x34, 0x5E]); // note: stack grows downward; top at lowest address
    cpu.step(&mut mem, false); // PULS
    assert_eq!(cpu.cycles, (5 + 4) + (5 + 4));
    assert_eq!(cpu.r.cc, 0xAA);
    assert_eq!(cpu.r.a, 0x12);
    assert_eq!(cpu.r.b, 0x34);
    assert_eq!(cpu.r.dp, 0x5E);
}

// SWI/RTI path test omitted: vectors live in top memory while our tests use RAM-only.
