//! Idle process: girato quando nessun altro task e' pronto.
//!
//! Non fa nulla se non fermare la CPU (`hlt`) finche' non arriva il prossimo
//! interrupt. Gli interrupt sono abilitati: appena il timer o la tastiera
//! interrompono, l'handler decide se passare il controllo a un altro
//! processo.

use core::arch::asm;

pub unsafe extern "C" fn idle() -> ! {
    loop {
        // hlt con interrupt abilitati; l'IRQ successivo riprenderà IL loop,
        // quindi lo scheduler avrà occasione di selezionare un altro task.
        unsafe { asm!("hlt") };
    }
}
