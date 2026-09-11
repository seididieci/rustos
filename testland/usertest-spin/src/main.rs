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

/// VA della pagina scratch nel processo (stessa per parent e figli: ogni
/// processo ha il proprio spazio, il VA non collide con heap/codice).
const SPIN_VA: u64 = 0x0000_4000_003C_0000;

unsafe fn counter_ptr() -> *mut u64 {
    SPIN_VA as *mut u64
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let my_pid = libr::getpid();
    let cfg: libr::IpcMsg = match libr::recv() {
        Ok(m) => m,
        Err(_) => libr::exit(1),
    };
    let parent = libr::CHANNEL_PARENT;
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
