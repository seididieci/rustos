//! PIT (Programmable Interval Timer 8253/8254): timer periodico ~100 Hz.
//!
//! Il PIT è il cuore dello scheduler: genera IRQ 0 a frequenza costante.
//! Il contatore `TICKS` è globale e leggibile dal scheduler (Fase 5) e
//! da qualsiasi codice kernel che debba misurare intervalli di tempo.
//!
//! Frequenza target: 100 Hz → ogni 10 ms circa.
//! Divisore = 1_193_182 / 100 = 11_931 (troncamento intero).

use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::port::Port;

const PIT_FREQUENCY: u32 = 1_193_182;
const TARGET_HZ: u32 = 100;
const DIVISOR: u16 = (PIT_FREQUENCY / TARGET_HZ) as u16;

/// Contatore globale di tick: incrementato ad ogni IRQ 0.
static TICKS: AtomicU64 = AtomicU64::new(0);

pub fn init() {
    let divisor = DIVISOR;
    unsafe {
        // Command byte: channel 0 | lobyte/hibyte | rate generator (mode 2) | binary
        Port::new(0x43).write(0x36u8);
        Port::new(0x40).write((divisor & 0xFF) as u8);
        Port::new(0x40).write((divisor >> 8) as u8);
    }

    crate::serial_println!(
        "[pit ] {} Hz (divisore {})",
        TARGET_HZ,
        DIVISOR
    );
}

/// Chiamato dall'IRQ 0 handler: incrementa il contatore di tick.
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

/// Ritorna il numero di tick trascorsi dall'avvio del PIT.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}
