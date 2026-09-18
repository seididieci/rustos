//! userdevfs — Device file server (Fase 9.3 + 9.6).
//!
//! Gestisce `/dev/null` e `/dev/zero`. Si registra presso userfs all'avvio con
//! la IPC FS_REGISTER (prefix="/dev/null" + "/dev/zero", via `libr::fs_register`). userfs instrada
//! le richieste di apertura/lettura/scrittura/chiusura verso questo processo e
//! mappa la pagina FS del client a `USER_FS_BUFFER` in questo processo prima di
//! inoltrarle: i dati (write) sono letti da li', i risultati (read/readdir)
//! scritti li' — direttamente nella pagina del client (zero-copy, Fase 9.6).

#![no_std]
#![no_main]

extern crate alloc;
use alloc::collections::BTreeMap;
use libr;

// ── IPC tags + device types (DocsD: single source in `syscall-numbers`) ─
use libr::{DEV_CLOSE, DEV_NULL, DEV_OPEN, DEV_READ, DEV_READDIR, DEV_WRITE, DEV_ZERO};

// ── Ring I/O (Fase 10.2) ─────────────────────────────────────────
// La response ring del client e' mappata a RESP_RING_VA da userfs (map_in);
// la request ring a REQ_RING_VA (usata per consumare i frame dei WRITE).
// Frame helpers in `libr` (A2).

const REQ_RING_VA: u64 = libr::CLI_REQ_VA;
const RESP_RING_VA: u64 = libr::CLI_RESP_VA;
// Geometria ring + errore IPC (A1): single source in `libr`.
use libr::ERR;
use libr::{req_frame_consume, resp_frame_write};

// ── Device table ────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum DeviceType { Null, Zero }

struct FdInfo { device: DeviceType }

struct DevTable {
    fds: BTreeMap<u32, FdInfo>,
    next_fd: u32,
}

impl DevTable {
    fn new() -> Self { Self { fds: BTreeMap::new(), next_fd: 1 } }

    fn open(&mut self, device: DeviceType) -> u64 {
        let fd = self.next_fd;
        self.next_fd += 1;
        self.fds.insert(fd, FdInfo { device });
        fd as u64
    }

    fn close(&mut self, fd: u32) -> bool {
        self.fds.remove(&fd).is_some()
    }

    fn get(&self, fd: u32) -> Option<DeviceType> {
        self.fds.get(&fd).map(|f| f.device)
    }
}

// ── Entry point ─────────────────────────────────────────────────────

use libr::println;

/// Assicura i mount "/dev/null" + "/dev/zero" presso userfs (Fase 14, t28;
/// prefix espliciti per-device da 16d, come ogni altro driver: niente
/// ombrello "/dev", cosi' il listing dei padri e' sintetizzato da userfs
/// dalla Mount table): attende Fs via soli lookup (NESSUN frame scritto
/// finche' userfs non c'e': niente spam nel ring che disallineerebbe gli
/// altri client), poi UN tentativo di registrazione per prefix; se fallisce
/// (race: userfs rimorto nel mentre) ricomincia dal lookup.
/// Stessa funzione a boot e su EXIT_NOTIFY: boot e restart sono la stessa
/// condizione ("Fs non c'e'"). Unbounded come `fs_chan`: senza Fs il driver
/// e' comunque inutile. Idempotente grazie al replace-on-register in userfs.
fn ensure_mounted() -> bool {
    // Prima i PROPRI ring: le injection map_in di userfs li hanno sovrascritti
    // (stessa VA condivisa, mai ripristinata) — senza remap scriveremmo nelle
    // pagine di un altro client (t28). No-op se mai allocati (fs_register poi
    // alloca via fs_init).
    let _ = libr::fs_remap_self();
    loop {
        while libr::service_lookup(libr::Service::Fs).is_err() {
            for _ in 0..1_000_000 {
                core::hint::spin_loop();
            }
        }
        if libr::fs_register_multi(&[b"/dev/null", b"/dev/zero"]) == 0 {
            return true;
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println!("[userdevfs] starting, pid={}", libr::getpid());

    // Registra il servizio Devfs per nome (ADR-0008).
    if libr::service_register(libr::Service::Devfs).is_ok() {
        println!("[userdevfs] registered as service Devfs");
    }

    // Registra i prefix "/dev/null" + "/dev/zero" presso userfs (unbounded:
    // senza Fs il driver e' comunque inutile; init ha gia' atteso userfs
    // pronto, quindi riesce subito a boot).
    ensure_mounted();
    println!("[userdevfs] registered /dev/null + /dev/zero with userfs");

    // Avvisa il parent (init) di essere pronto (SVC_READY, come userfs):
    // serve al supervisore init-restart per l'attesa prontezza (Fase 14).
    // Fire-and-forget (send_async): a boot init non aspetta devfs → una
    // send sync resterebbe bloccata per sempre. Retry bounded, mai hang.
    for _ in 0..100 {
        if libr::send_async(libr::CHANNEL_PARENT, libr::SVC_READY, 1, 0).is_ok() {
            break;
        }
        for _ in 0..10_000 {
            core::hint::spin_loop();
        }
    }

    let mut devtable = DevTable::new();

    loop {
        let msg = match libr::recv() {
            Ok(m) => m,
            Err(_) => continue,
        };

        // userfs morto e rinato (t28): re-mount. L'unico peer mortale e'
        // userfs: ricontrolla incondizionato (idempotente). Mai reply.
        if msg.tag == libr::EXIT_NOTIFY {
            println!("[userdevfs] peer morto, re-mount /dev/null + /dev/zero");
            ensure_mounted();
            continue;
        }

        let result: Option<u64> = match msg.tag {
            DEV_OPEN => {
                match msg.w0 {
                    DEV_NULL => Some(devtable.open(DeviceType::Null)),
                    DEV_ZERO => Some(devtable.open(DeviceType::Zero)),
                    _ => None,
                }
            }

            DEV_READ => {
                let fd = msg.w0 as u32;
                let count = msg.w1 as usize;
                match devtable.get(fd) {
                    Some(DeviceType::Null) => {
                        // EOF: scrivi comunque un frame vuoto (result 0) cosi' il
                        // client vede 0 byte letti (EOF), non un ring vuoto (-1).
                        unsafe { resp_frame_write(RESP_RING_VA, &[]); }
                        Some(0)
                    }
                    Some(DeviceType::Zero) => {
                        let n = count.min(4096);
                        let zeros = [0u8; 4096];
                        unsafe { resp_frame_write(RESP_RING_VA, &zeros[..n]); }
                        Some(n as u64)
                    }
                    None => None,
                }
            }

            DEV_WRITE => {
                // Il payload del WRITE e' nel request ring del client (mappato
                // a REQ_RING_VA). /dev/null e /dev/zero scartano i dati, ma la
                // tail va consumata o il prossimo request del client e' male.
                let count = msg.w1 as usize;
                unsafe { req_frame_consume(REQ_RING_VA, count); }
                Some(count as u64)
            }

            DEV_CLOSE => {
                if devtable.close(msg.w0 as u32) { Some(0) } else { None }
            }

            DEV_READDIR => {
                let mut buf = [0u8; 12];
                let null_entry = b"null\0";
                let zero_entry = b"zero\0";
                let mut pos = 0;
                for entry in [&null_entry[..], &zero_entry[..]] {
                    let len = entry.len();
                    if pos + len <= buf.len() {
                        let dest = &mut buf[pos..pos + len];
                        dest.copy_from_slice(entry);
                        pos += len;
                    }
                }
                unsafe { resp_frame_write(RESP_RING_VA, &buf[..pos]); }
                Some(2)
            }

            _ => None,
        };

        let _ = libr::reply(0, result.unwrap_or(ERR), 0);
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    println!("[userdevfs] panic");
    libr::exit(1)
}
