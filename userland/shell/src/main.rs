//! usershell — Shell interattiva per Velordor (Fase 9.4, utility Fase 18).
//!
//! Client del terminale: NON mappa il VGA. Apre `/dev/input/keyboard` e usa lo
//! stesso fd per leggere i tasti (read) e per scrivere l'output (write): il
//! console server possiede la VGA, disegna l'output e fa l'echo dei tasti
//! (Opzione B). La shell gestisce solo la linea logica dei comandi.
//! Comandi: ls, cat, touch, mkdir, mount, umount, echo, clear, wc, hexdump,
//! kill, cd, pwd, cp, mv, rm, rmdir, exit, help. Tutti i path passano per
//! `resolve()`: la shell tiene una cwd client-side e accetta path relativi
//! (Fase 18.1).

#![no_std]
#![no_main]

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::vec;
use libr;

mod cmd_fs;
mod cmd_info;
mod cmd_run;
mod cwd;
mod repl;
mod term;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    let _ = libr::print_string(b"[shell] panic\n");
    libr::exit(1)
}
