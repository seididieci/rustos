//! usertestspin — helper busy-loop della test suite (testland).
//!
//! Spins in ring 3 per un budget di `get_ticks` (PIT) ricevuto via IPC CFG,
//! quindi `send(T_DONE)` all'orchestratore. Modalita':
//!   - w1=0: solo timed (usato per il test di priorita').
//!   - w1=1: mappa anche la pagina scratch `MAP_TEST_PHYS` e incrementa un
//!     contatore a ogni cambio di tick osservato: l'orchestratore, che gira in
//!     parallelo, lo legge mentre è bloccato/spinning → prova che il timer lo
//!     ha preemptato (scheduler RR tra processi Normal).
//!
//! Stesso binario esposto a piu' priorita' (utspin_norm/high, usertestspin
//! Low) tramite piu' righe in `user_binary.rs::NAMED_BINARIES`.

#![no_std]
#![no_main]

use libr::println;

const T_ACK: u64 = 101;
const T_DONE: u64 = 103;
const T_READY: u64 = 107;

// Fase 36 (t51): `w0` magico = sonda di squat. Binario DIVERSO da testcli
// (hash diverso per costruzione): tenta FS_REGISTER su "/dev/t51" — deve
// essere RIFIUTATO (prefix vivo di un altro binario, noi non figli di init)
// — e serve DEV_OPEN al minimo, cosi' il test distingue "rifiutato" (il mount
// resta di X) da "accettato" (staremmo servendo noi). Poi parcheggiato in
// recv finche' il parent killa (costo zero, come KILLME).
const SQUAT_MAGIC: u64 = 0x5351_5541_5435_31; // "SQUAT51" ASCII

/// VA della pagina scratch nel processo (stessa per parent e figli: ogni
/// processo ha il proprio spazio, il VA non collide con heap/codice).
const SPIN_VA: u64 = 0x0000_4000_003C_0000;

unsafe fn counter_ptr() -> *mut u64 {
    SPIN_VA as *mut u64
}

libr::entry!(real_main);
fn real_main(_sp: u64) -> ! {
    let my_pid = libr::getpid();
    let cfg: libr::IpcMsg = match libr::recv() {
        Ok(m) => m,
        Err(_) => libr::exit(1),
    };
    let parent = libr::CHANNEL_PARENT;
    if cfg.w0 == SQUAT_MAGIC {
        let _ = libr::reply(T_ACK, 1, 0);
        // Esito ignorato di proposito: lo squat DEVE fallire (il test lo
        // osserva dal routing, la reply di FS_REGISTER e' sempre ok).
        let _ = libr::fs_register(b"/dev/t51");
        if libr::send(parent, T_READY, 1, 0).is_err() {
            libr::exit(1);
        }
        loop {
            match libr::recv() {
                Ok(m) if m.tag == libr::DEV_OPEN => {
                    let _ = libr::reply(0, 1, 0);
                }
                Ok(m) if m.tag == libr::DEV_CLOSE => {
                    let _ = libr::reply(0, 0, 0);
                }
                Ok(_) => {
                    let _ = libr::reply(T_ACK, 0, 0);
                }
                Err(_) => {}
            }
        }
    }
    let budget = if cfg.w0 == 0 { 20 } else { cfg.w0 as i64 };
    let progress = cfg.w1 == 1;

    let _ = libr::reply(T_ACK, libr::getpid() as u64, 0);

    // Flag: scratch page mappata con successo. Il loop accede a SPIN_VA SOLO
    // se mapped: un accesso a pagina non mappata in user mode = page fault.
    let mut mapped = false;
    if progress {
        mapped = libr::map_physical(libr::MAP_TEST_PHYS, SPIN_VA, 1).is_ok();
        if !mapped {
            println!("[utspin] pid={} WARNING: map_physical FAILED", my_pid);
        }
        if mapped {
            unsafe { core::ptr::write_volatile(counter_ptr(), 0) };
        }
    }

    let t0 = libr::get_ticks();
    let mut last = t0;
    let mut observed = 0i64;
    // Batch: tra due get_ticks (syscall) gira ~512 iterazioni di spin puro.
    // Un busy-loop che chiama get_ticks a ogni iterazione maschera gli
    // interrupt (IF=0 durante la syscall) e affama il timer: il PIT non
    // preempta, il wall-clock non avanza e il budget non scade mai (vedi
    // AGENTS, robustezza scheduler). Con il batch il processo sta in user
    // mode con IF=1 quasi tutto il tempo → il timer scatta regolarmente.
    loop {
        for _ in 0..512 {
            core::hint::spin_loop();
        }
        let now = libr::get_ticks();
        if now != last {
            last = now;
            observed += 1;
            if mapped {
                let c = unsafe { core::ptr::read_volatile(counter_ptr()) };
                unsafe { core::ptr::write_volatile(counter_ptr(), c + 1) };
            }
        }
        if now - t0 >= budget {
            break;
        }
    }

    let _ = libr::send(parent, T_DONE, 1, observed as u64);
    println!("[utspin] pid={} done budget={} observed={}", my_pid, budget, observed);
    libr::exit(0);
}

#[panic_handler]
fn panic_handler(_info: &core::panic::PanicInfo) -> ! {
    println!("[utspin] panic");
    libr::exit(1)
}
