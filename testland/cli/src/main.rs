//! Client IPC user (Fase 7): invia richieste al server e stampa le risposte.
//!
//! Ciclo:
//!   1. `send(server_pid, tag=REQ, w0, w1)` → il processo viene bloccato dal
//!      kernel finche' il server non risponde.
//!   2. Al risveglio, `send` restituisce la risposta: la logga (`[cli] reply ..`).
//!
//! Il pid del server: i processi sono creali in ordine (Fase 7) → il server e'
//! spawnato subito prima del client, quindi `server_pid = getpid() - 1`.

#![no_std]
#![no_main]

use libr;
use libr::{println};

const TAG_REQ: u64 = 7;

/// Entry del client: invia richieste in loop e stampa le risposte.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let my_pid = libr::getpid();
    let server_pid = my_pid - 1;

    println!("[cli] client up, pid={}, server_pid={}", my_pid, server_pid);

    let mut counter: u64 = 1;
    loop {
        match libr::send(server_pid as usize, TAG_REQ, counter, 0) {
            Ok(r) => {
                println!("[cli] reply tag={} w0={} w1={}", r.tag, r.w0, r.w1);
            }
            Err(()) => {
                println!("[cli] send failed");
            }
        }
        counter += 1;
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    println!("[cli] panic");
    libr::exit(1)
}
