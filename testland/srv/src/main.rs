//! Server IPC user (Fase 7 → ADR-0008): server di prova in ring 3 che fa
//! `recv` + `reply` (ADR-0008).
//!
//! Ciclo:
//!   1. `recv()` bloccante: attende un messaggio dal client.
//!   2. Logga il messaggio ricevuto.
//!   3. `reply(w0*2, w1*2)`: risponde al mittente del messaggio corrente.

#![no_std]
#![no_main]

use libr;
use libr::{println};

/// Entry del server: riceve e risponde in loop.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println!("[srv] server up, pid={}", libr::getpid());

    loop {
        match libr::recv() {
            Ok(msg) => {
                println!("[srv] recv chan={} req={} tag={} w0={} w1={}",
                    msg.channel, msg.request_id, msg.tag, msg.w0, msg.w1);
                // Echo trasformato: raddoppia w0/w1 come prova di risposta.
                let _ = libr::reply(0, msg.w0 * 2, msg.w1 * 2);
                println!("[srv] replied");
            }
            Err(()) => {
                println!("[srv] recv failed");
            }
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    println!("[srv] panic");
    libr::exit(1)
}
