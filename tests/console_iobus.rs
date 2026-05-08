use em6809_core::bus::{Bus, Memory};
use em6809_core::io::{IoBus, Mc6850Dev};

// MC6850 ACIA register layout:
//   base+0  read = Status (bit 0 = RDRF, bit 1 = TDRE, bit 7 = IRQ)
//           write = Control Register (CR)
//   base+1  read = RX data; write = TX data

#[test]
fn console_rx_tx_status() {
    let mem = Memory::new();
    let mut bus = IoBus::new(mem);
    let mut con = Mc6850Dev::new(0xFF00);
    con.feed_bytes(b"Hi");
    bus.add_device(con);

    // STATUS should show RDRF=1 (bit 0) when RX has data; TDRE always set in
    // this instant-transmit emulation.
    let st = bus.read8(0xFF00);
    assert_ne!(st & 0x01, 0x00, "RDRF should be set when RX has data");
    assert_ne!(st & 0x02, 0x00, "TDRE should be set (instant transmit)");

    // Read two bytes from RX data register at +1.
    let h = bus.read8(0xFF01);
    let i = bus.read8(0xFF01);
    assert_eq!(h, b'H');
    assert_eq!(i, b'i');

    // RX empty → RDRF clear.
    let st2 = bus.read8(0xFF00);
    assert_eq!(st2 & 0x01, 0x00, "RDRF should be clear when RX is empty");

    // TX: write a byte to the data register at +1 (prints; just verify no panic).
    bus.write8(0xFF01, b'!');
}

#[test]
fn console_rx_irq_watermark() {
    // Below the RX watermark, no IRQ asserts.
    let mem = Memory::new();
    let mut bus = IoBus::new(mem);
    let mut con = Mc6850Dev::new(0xFF00);
    con.set_rx_watermark(2);
    con.feed_bytes(b"A");
    bus.add_device(con);
    let (_n, _f, irq1) = bus.irq_lines();
    assert!(!irq1, "Below watermark should not assert IRQ");

    // At/above watermark with RIE enabled, IRQ asserts.
    let mem2 = Memory::new();
    let mut bus2 = IoBus::new(mem2);
    let mut con2 = Mc6850Dev::new(0xFF00);
    con2.set_rx_watermark(2);
    con2.feed_bytes(b"AB");
    // Enable RIE via CR write at base+0.  CDS bits 0-1 = 00 (no master reset),
    // bit 7 = 1 enables receive interrupt.
    em6809_core::io::Device::write8(&mut con2, 0xFF00, 0x80);
    bus2.add_device(con2);
    let (_n2, _f2, irq2) = bus2.irq_lines();
    assert!(irq2, "At/above watermark with RIE should assert IRQ");
}
