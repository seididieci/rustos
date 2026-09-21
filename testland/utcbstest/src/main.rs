//! utcbstest — helper CBS bandwidth test (testland).
//!
//! Riceve parametri CBS (budget, period) via IPC CFG, crea un server CBS,
//! lo attacca a se stesso, e fa busy-loop per N tick contando i tick
//! osservati su una pagina scratch condivisa. Invia T_DONE al parent
//! con il conteggio osservato.
//!
//! Per la modalita' "hog" (busy-loop senza CBS), si usa utspin_norm.

#![no_std]
#![no_main]

use libr::println;

const T_ACK: u64 = 101;
const T_DONE: u64 = 103;

/// VA della pagina scratch nel processo.
const SPIN_VA: u64 = 0x0000_4000_003C_0000;

/// Durata del busy-loop in tick (PIT 100 Hz → 200 tick = 2 secondi).
const SPIN_TICKS: i64 = 200;

unsafe fn counter_ptr() -> *mut u64 {
    SPIN_VA as *mut u64
}

libr::entry!(real_main);
fn real_main(_sp: u64) -> ! {
    let cfg: libr::IpcMsg = match libr::recv() {
        Ok(m) => m,
        Err(_) => libr::exit(1),
    };
    let parent = libr::CHANNEL_PARENT;
    let budget = cfg.w0 as u32;  // Q: budget in tick
    let period = cfg.w1 as u32;  // P: periodo in tick

    let my_pid = libr::getpid();

    // Rispondi subito al parent (spawn_cfg si blocca in attesa di ACK).
    let _ = libr::reply(T_ACK, my_pid as u64, 0);

    // Crea il server CBS.
    let server_id = match libr::cbs_create(budget, period) {
        Ok(id) => id,
        Err(()) => {
            println!("[utcbstest] cbs_create FAILED (Q={} P={})", budget, period);
            let _ = libr::send(parent, T_DONE, 0, 0);
            libr::exit(1);
        }
    };

    // Attacca il server a se stesso.
    if libr::cbs_attach(server_id).is_err() {
        println!("[utcbstest] cbs_attach FAILED (server={})", server_id);
        let _ = libr::send(parent, T_DONE, 0, 0);
        libr::exit(1);
    }

    println!("[utcbstest] pid={} CBS Q={} P={} bw={:.0}% server={}",
        my_pid, budget, period,
        budget as f64 / period as f64 * 100.0, server_id);

    // Mappa pagina scratch e inizializza contatore.
    if libr::map_physical(libr::MAP_TEST_PHYS, SPIN_VA, 1).is_ok() {
        unsafe { core::ptr::write_volatile(counter_ptr(), 0) };
    }

    // Busy-loop: conta i tick osservati. Batch da 512 spin puri tra due
    // get_ticks (syscall): un get_ticks a ogni iterazione maschera IF e
    // affama il timer → il wall-clock non avanza e il loop non scade.
    let t0 = libr::get_ticks();
    let mut observed = 0i64;
    let mut last = t0;
    loop {
        for _ in 0..512 {
            core::hint::spin_loop();
        }
        let now = libr::get_ticks();
        if now != last {
            last = now;
            observed += 1;
            // Aggiorna contatore sulla pagina scratch (il parent lo legge).
            unsafe { core::ptr::write_volatile(counter_ptr(), observed as u64) };
        }
        if now - t0 >= SPIN_TICKS {
            break;
        }
    }

    println!("[utcbstest] pid={} done observed={}/{}", my_pid, observed, SPIN_TICKS);
    let _ = libr::send(parent, T_DONE, 1, observed as u64);
    libr::exit(0);
}

#[panic_handler]
fn panic_handler(_info: &core::panic::PanicInfo) -> ! {
    println!("[utcbstest] panic");
    libr::exit(1)
}
