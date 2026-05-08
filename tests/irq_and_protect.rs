use em6809_core::bus::{Bus, Memory};
use em6809_core::io::{Device, IoBus, Mc6850Dev};
use em6809_core::mmu::Mc6829;

#[test]
fn mmu_write_protect_raises_irq() {
    let mut mmu = Mc6829::new(1 << 20, 0xFFA0);
    mmu.identity_map_current();
    // Enable write-protect on page 0 and IRQ on fault
    // attr page0 at regs+0x18
    mmu.write8(0xFFA0 + 0x18, 0x01);
    mmu.write8(0xFFA0 + 0x12, 0x03); // prot_ctrl: WPROT_EN + IRQ on fault
                                     // Attempt to write logical 0x0001
    mmu.write8(0x0001, 0xAA);
    let (_n, _f, irq) = mmu.irq_lines();
    assert!(irq, "IRQ should be raised on write-protect fault");
}

#[test]
fn console_rx_irq_line() {
    let mem = Memory::new();
    let mut bus = IoBus::new(mem);
    let mut con = Mc6850Dev::new(0xFF00);
    con.feed_bytes(b"A");
    // Enable RIE on the MC6850 control register at base+0 (bit 7).
    Device::write8(&mut con, 0xFF00, 0x80);
    bus.add_device(con);
    let (_n, _f, irq) = bus.irq_lines();
    assert!(irq, "Console RX should assert IRQ when RIE is set");
}

#[test]
fn mmu_noexec_fetch_fault_sets_irq() {
    let mut mmu = Mc6829::new(1 << 20, 0xFFA0);
    mmu.identity_map_current();
    // Page 0 NX, enable NX_EN and IRQ on fault
    mmu.write8(0xFFA0 + 0x18, 0x04); // attr: NX
    mmu.write8(0xFFA0 + 0x12, 0x0A); // prot_ctrl: NX_EN + IRQ on fault
                                     // Instruction fetch from 0x0000 should trip fault
    let _ = mmu.read8_fetch(0x0000);
    let (_n, _f, irq) = mmu.irq_lines();
    assert!(irq, "NX fetch should assert IRQ when enabled");
}

// MC6850 ACIA only routes its single interrupt line to the host's IRQ input
// — there is no FIRQ mode on the chip.  The Simple console device that used
// to support FIRQ has been removed, so the matching test was deleted along
// with it.
