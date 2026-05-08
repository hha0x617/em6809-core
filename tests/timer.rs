use em6809_core::bus::{Bus, Memory};
use em6809_core::io::IoBus;
use em6809_core::timer::TimerDev;

#[test]
fn timer_irq_asserts_on_wrap_and_clear_pending() {
    // Attach timer at $FF10 on an IoBus over plain memory
    let mem = Memory::new();
    let mut bus = IoBus::new(mem);
    let mut t = TimerDev::new(0xFF10);
    t.set_reload(3); // small period in instruction ticks
    t.set_irq_enable(true);
    t.start();
    bus.add_device(t);

    // Advance a few ticks; expect IRQ to assert when the counter wraps
    let mut saw_irq = false;
    for _ in 0..16 {
        let (_n, _f, i) = bus.irq_lines();
        if i {
            saw_irq = true;
            break;
        }
    }
    assert!(saw_irq, "Timer should assert IRQ after wrap");

    // PENDING bit should be visible in CTRL/STATUS read (bit3)
    let status = bus.read8(0xFF10);
    assert_ne!(status & 0x08, 0, "PENDING bit should be set");

    // Clear pending by writing CTRL with bit4 set; keep RUN+IRQ_EN bits
    bus.write8(0xFF10, 0x10 | 0x03);

    // After clearing, the next irq_lines() should be false (until next wrap)
    let (_n2, _f2, i2) = bus.irq_lines();
    assert!(!i2, "IRQ should clear after writing bit4 in CTRL");
}

#[test]
fn timer_firq_mode_routes_to_firq_line() {
    let mem = Memory::new();
    let mut bus = IoBus::new(mem);
    let mut t = TimerDev::new(0xFF10);
    t.set_reload(2);
    t.set_irq_enable(true);
    t.set_firq(true);
    t.start();
    bus.add_device(t);

    // Spin until the device asserts; it should route to FIRQ, not IRQ
    let mut firq_on = false;
    for _ in 0..16 {
        let (_n, f, i) = bus.irq_lines();
        if f && !i {
            firq_on = true;
            break;
        }
    }
    assert!(firq_on, "Timer in FIRQ mode should assert FIRQ line only");
}
