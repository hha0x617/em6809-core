use em6809_core::io::{BlockDev, Device};

#[test]
fn read_out_of_range_returns_zeros_and_sets_err() {
    let mut dev = BlockDev::new(0xFF20);
    dev.set_image(vec![0xAA; 512]); // only 1 sector present
                                    // Set LBA to 1 (out of range), SECCNT=1
    dev.write8(0xFF23, 1); // SECCNT
    dev.write8(0xFF24, 1); // LBA0
    dev.write8(0xFF25, 0); // LBA1..3
    dev.write8(0xFF26, 0);
    dev.write8(0xFF27, 0);
    dev.write8(0xFF22, 0x01); // READ
                              // STATUS should indicate DRDY and ERR on first read (also clears ERR)
    let st = dev.read8(0xFF21);
    assert_ne!(st & 0x02, 0, "DRDY set");
    assert_ne!(st & 0x04, 0, "ERR set on first STATUS read for OOB");
    // Read a few bytes; all zeros
    for _ in 0..16 {
        let b = dev.read8(0xFF20);
        assert_eq!(b, 0x00);
    }
    // ERR gets cleared by reading STATUS
    let st2 = dev.read8(0xFF21);
    assert_eq!(st2 & 0x04, 0, "ERR cleared after STATUS read");
}

#[test]
fn write_then_read_roundtrip_multi_sector() {
    let mut dev = BlockDev::new(0xFF20);
    dev.set_irq_enable(false);
    // Prepare two-sector write starting at LBA=2
    dev.write8(0xFF23, 2); // SECCNT=2
    dev.write8(0xFF24, 2); // LBA=2
    dev.write8(0xFF25, 0);
    dev.write8(0xFF26, 0);
    dev.write8(0xFF27, 0);
    dev.write8(0xFF22, 0x02); // WRITE start
                              // Write 1024 bytes of pattern
    for i in 0..1024u32 {
        let v = (i & 0xFF) as u8;
        dev.write8(0xFF20, v);
    }
    // Read them back
    dev.write8(0xFF23, 2); // SECCNT=2
    dev.write8(0xFF24, 2); // LBA=2
    dev.write8(0xFF25, 0);
    dev.write8(0xFF26, 0);
    dev.write8(0xFF27, 0);
    dev.write8(0xFF22, 0x01); // READ
    let mut ok = true;
    for i in 0..1024u32 {
        let v = dev.read8(0xFF20);
        if v != (i & 0xFF) as u8 {
            ok = false;
            break;
        }
    }
    assert!(ok, "roundtrip pattern matches");
}

#[test]
fn irq_pulses_on_read_completion_when_enabled() {
    let mut dev = BlockDev::new(0xFF20);
    dev.set_irq_enable(true);
    dev.set_irq_hold_cycles(4);
    // one sector at LBA=0, but no image; still should produce DRDY+ERR on read
    dev.write8(0xFF23, 1);
    dev.write8(0xFF24, 0);
    dev.write8(0xFF25, 0);
    dev.write8(0xFF26, 0);
    dev.write8(0xFF27, 0);
    dev.write8(0xFF22, 0x01);
    // While DRDY, irq_lines should report asserted
    let (_n, _f, i) = dev.irq_lines();
    assert!(i, "IRQ asserted on DRDY when enabled");
    // Drain entire sector
    for _ in 0..512 {
        let _ = dev.read8(0xFF20);
    }
    // After completion, hold should keep IRQ asserted for some calls
    let mut any_on = false;
    for _ in 0..5 {
        let (_n2, _f2, i2) = dev.irq_lines();
        any_on |= i2;
    }
    assert!(any_on, "IRQ hold produced post-completion assertion");
}
