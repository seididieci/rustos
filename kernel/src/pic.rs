//! PIC 8259: inizializzazione, remapping e EOI.
//!
//! Il PIC seriale gestisce gli interrupt hardware. Dopo il boot è in uno
//! stato sconosciuto: va remappato per non sovrapporsi alle eccezioni CPU
//! (0-31) e poi mascherato, abilitando solo le IRQ necessarie.
//!
//! Layout remappato (ADR-0005, Fase 3):
//!   master IRQ 0-7  → INT 0x20-0x27
//!   slave  IRQ 8-15 → INT 0x28-0x2F

use pic8259::ChainedPics;
use spin::Mutex;

/// Offset del master PIC: IRQ 0-7 → INT 32-39.
pub const PIC_1_OFFSET: u8 = 0x20;
/// Offset dello slave PIC: IRQ 8-15 → INT 40-47.
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

pub fn init() {
    unsafe { PICS.lock().initialize() };

    // Maschera tutte le IRQ tranne IRQ 0 (timer), IRQ 1 (keyboard) e
    // IRQ 14/15 (ATA primario/secondario, Fase 38: notify a userdisk; col PIO
    // attuale sono spurie e tollerate, col DMA diventano il completamento).
    // Bit 0 = IRQ0, ... bit 7 = IRQ7; 1 = masked. IRQ2 (cascade) resta aperta.
    unsafe {
        PICS.lock().write_masks(0b1111_1100, 0b0011_1111);
    }

    crate::serial_println!(
        "[pic ] remappata: master {:#x}, slave {:#x} (IRQ 0+1+14+15 abilitate)",
        PIC_1_OFFSET,
        PIC_2_OFFSET
    );
}

/// Manda End of Interrupt al PIC dopo aver servito un IRQ.
///
/// # Sicurezza
/// `irq` e' il numero INT (vettore: 0x20 per IRQ0, 0x21 per IRQ1 con gli
/// offset sopra), come vuole `pic8259::notify_end_of_interrupt` — non il
/// numero IRQ 0-15.
pub unsafe fn end_of_interrupt(irq: u8) {
    unsafe { PICS.lock().notify_end_of_interrupt(irq) };
}
