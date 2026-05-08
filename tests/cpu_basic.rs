use em6809_core::bus::Memory;
use em6809_core::cpu::Cpu; // if binary crate, expose modules via lib or use path

#[test]
fn lda_imm_cycles() {
    let mut mem = Memory::new();
    mem.load_slice(0x0000, &[0x86, 0x5A]); // LDA #$5A
    let mut cpu = Cpu::new();
    cpu.set_pc(0x0000);
    let c0 = cpu.cycles;
    cpu.step(&mut mem, false);
    let c1 = cpu.cycles;
    assert_eq!(cpu.r.a, 0x5A);
    assert_eq!(c1 - c0, 2);
}

#[test]
fn shifts_acc_flags() {
    let mut mem = Memory::new();
    // ASLA; LSRA; ROLA; RORA sequence on A starting 0x81 with C=1
    mem.load_slice(0x0000, &[0x48, 0x44, 0x49, 0x46]);
    let mut cpu = Cpu::new();
    cpu.set_pc(0x0000);
    cpu.r.a = 0x81;
    cpu.r.cc |= 0x01; // C=1
    cpu.step(&mut mem, false); // ASLA -> 0x02, C=1
    assert_eq!(cpu.r.a, 0x02);
    assert_eq!(cpu.r.cc & 0x01, 0x01);
    cpu.step(&mut mem, false); // LSRA -> 0x01, C=0
    assert_eq!(cpu.r.a, 0x01);
    assert_eq!(cpu.r.cc & 0x01, 0x00);
    cpu.step(&mut mem, false); // ROLA -> 0x02, C=0
    assert_eq!(cpu.r.a, 0x02);
    assert_eq!(cpu.r.cc & 0x01, 0x00);
    cpu.step(&mut mem, false); // RORA with C_in=0 -> 0x01, C=0
    assert_eq!(cpu.r.a, 0x01);
    assert_eq!(cpu.r.cc & 0x01, 0x00);
}

#[test]
fn addd_subd_cycles() {
    let mut mem = Memory::new();
    // ADDD #$0001; SUBD #$0002
    mem.load_slice(0x0000, &[0xC3, 0x00, 0x01, 0x83, 0x00, 0x02]);
    let mut cpu = Cpu::new();
    cpu.set_pc(0x0000);
    cpu.r.a = 0x12;
    cpu.r.b = 0x34; // D=0x1234
    cpu.step(&mut mem, false);
    assert_eq!(((cpu.r.a as u16) << 8) | cpu.r.b as u16, 0x1235);
    let c1 = cpu.cycles;
    cpu.step(&mut mem, false);
    assert_eq!(((cpu.r.a as u16) << 8) | cpu.r.b as u16, 0x1233);
    assert_eq!(c1, 4);
    assert_eq!(cpu.cycles, 8);
}

#[test]
fn lda_sta_direct_cycles() {
    let mut mem = Memory::new();
    mem.load_slice(0x0000, &[0x86, 0x42, 0x97, 0x10]); // LDA #$42; STA <$10
    let mut cpu = Cpu::new();
    cpu.set_pc(0x0000);
    cpu.r.dp = 0x00;
    cpu.step(&mut mem, false);
    cpu.step(&mut mem, false);
    assert_eq!(mem.read_slice(0x0010, 1)[0], 0x42);
    assert_eq!(cpu.cycles, 2 + 4);
}

#[test]
fn ldx_jsr_rts_cycles() {
    let mut mem = Memory::new();
    mem.load_slice(0x0000, &[0xBD, 0x00, 0x10]); // JSR $0010
    mem.load_slice(0x0010, &[0x39]); // RTS
    let mut cpu = Cpu::new();
    cpu.set_pc(0x0000);
    cpu.step(&mut mem, false); // JSR
    assert_eq!(cpu.r.pc, 0x0010);
    cpu.step(&mut mem, false); // RTS
    assert_eq!(cpu.r.pc, 0x0003);
    assert_eq!(cpu.cycles, 7 + 5);
}

#[test]
fn lda_indexed_5x_cycles() {
    let mut mem = Memory::new();
    // LDX #$1000; LDA 5,X
    mem.load_slice(0x0000, &[0x8E, 0x10, 0x00, 0xA6, 0x05]);
    mem.load_slice(0x1005, &[0x99]);
    let mut cpu = Cpu::new();
    cpu.set_pc(0x0000);
    cpu.step(&mut mem, false); // LDX #
    cpu.step(&mut mem, false); // LDA 5,X
    assert_eq!(cpu.r.x, 0x1000);
    assert_eq!(cpu.r.a, 0x99);
    assert_eq!(cpu.cycles, 3 + 4);
}
