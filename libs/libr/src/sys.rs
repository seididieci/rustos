use super::*;

/// Scrive `count` byte da `buf` sul descrittore `fd`. Ritorna i byte scritti,
/// oppure un valore negativo in caso di errore (es. fd non supportato).
#[inline]
pub fn write(fd: u64, buf: *const u8, count: usize) -> i64 {
    unsafe { syscall4(SYS_WRITE, fd, buf as u64, count as u64, 0) }
}

/// Id del processo corrente.
#[inline]
pub fn getpid() -> i64 {
    unsafe { syscall4(SYS_GETPID, 0, 0, 0, 0) }
}

/// Numero di tick PIT trascorsi dall'avvio (100 Hz).
#[inline]
pub fn get_ticks() -> i64 {
    unsafe { syscall4(SYS_GET_TICKS, 0, 0, 0, 0) }
}

/// Attende `n` tick con batch di spin puri tra due `get_ticks` (pattern
/// robusto scheduler, A4: prima identico in init/usertests). Una `get_ticks`
/// per iterazione maschera IF=0 a ogni syscall e brucia quanti che rallentano
/// gli handoff IPC altrui; 512 spin puri lasciano IF=1 quasi tutto il tempo.
pub fn spin_ticks(n: i64) {
    let t0 = get_ticks();
    while get_ticks() - t0 < n {
        for _ in 0..512 {
            core::hint::spin_loop();
        }
    }
}

/// `sbrk(inc)`: estende l'heap del processo di `inc` byte (arrotondati a
/// pagina dal kernel; nessuna pagina mappata subito, materializzazione lazy
/// al primo accesso). Ritorna il vecchio `heap_brk` (inizio della nuova
/// regione), oppure `Err` se l'estensione non e' possibile.
#[inline]
pub fn sbrk(inc: usize) -> Result<usize, ()> {
    let r = unsafe { syscall4(SYS_SBRK, inc as u64, 0, 0, 0) };
    if r < 0 {
        Err(())
    } else {
        Ok(r as usize)
    }
}

/// Fase 28 — `mmap(hint, len)`: mappa anonima privata RW nel basso canonico
/// (zero-fill lazy come `sbrk`). `hint == 0` = scelta kernel (first-fit dal
/// basso); altrimenti e' un consiglio onorato solo se libero. Ritorna la base
/// (sempre < 2^63) o `Err`. Solo RW in 28 (il kernel rifiuta altri `prot`).
#[inline]
pub fn mmap(hint: usize, len: usize) -> Result<usize, ()> {
    let r = unsafe { syscall4(SYS_MMAP, hint as u64, len as u64, PROT_READ | PROT_WRITE, 0) };
    if r < 0 {
        Err(())
    } else {
        Ok(r as usize)
    }
}

/// Fase 28 — `mmap_fixed(addr, len)`: come `mmap` ma piazza esattamente ad
/// `addr` (`MMAP_FIXED`) o fallisce, mai fallback. Utile per riuso
/// deterministico dopo `munmap`.
#[inline]
pub fn mmap_fixed(addr: usize, len: usize) -> Result<usize, ()> {
    let r = unsafe { syscall4(SYS_MMAP, addr as u64, len as u64, PROT_READ | PROT_WRITE, MMAP_FIXED) };
    if r < 0 {
        Err(())
    } else {
        Ok(r as usize)
    }
}

/// Fase 28 — `munmap(addr, len)`: smappa VMA intere (parziali = `Err` senza
/// cambiare stato, niente split in 28).
#[inline]
pub fn munmap(addr: usize, len: usize) -> Result<(), ()> {
    let r = unsafe { syscall4(SYS_MUNMAP, addr as u64, len as u64, 0, 0) };
    if r < 0 {
        Err(())
    } else {
        Ok(())
    }
}

/// Fase 29 — `mmap_prot(hint, len, prot)`: come `mmap` ma con `prot` esplicito
/// (`PROT_NONE`/`PROT_READ`/`PROT_READ|PROT_WRITE`; altri = `Err`). Le pagine
/// vengono materializzate al primo accesso con i flag del prot (RO = scrittura
/// → fault → kill del processo).
#[inline]
pub fn mmap_prot(hint: usize, len: usize, prot: u64) -> Result<usize, ()> {
    let r = unsafe { syscall4(SYS_MMAP, hint as u64, len as u64, prot, 0) };
    if r < 0 {
        Err(())
    } else {
        Ok(r as usize)
    }
}

/// Fase 29 — `mprotect(addr, len, prot)`: cambia le protezioni di VMA intere
/// (copertura esatta come `munmap`; parziali = `Err` senza cambiare stato).
/// A `PROT_NONE` le pagine cadono (il riuso rimaterializza zero); RO↔RW flippa
/// il bit W. Il codice e' l'unico mapping eseguibile: niente PROT_EXEC.
#[inline]
pub fn mprotect(addr: usize, len: usize, prot: u64) -> Result<(), ()> {
    let r = unsafe { syscall4(SYS_MPROTECT, addr as u64, len as u64, prot, 0) };
    if r < 0 {
        Err(())
    } else {
        Ok(())
    }
}

/// Fase 30 — `shm_create(len)`: crea una regione di memoria condivisa di
/// `len` byte (frame contigui azzerati, max 256 KiB) e ritorna l'id (>= 1).
/// L'id si passa a un altro processo via IPC, che la mappa con `shm_map`.
#[inline]
pub fn shm_create(len: usize) -> Result<u32, ()> {
    let r = unsafe { syscall4(SYS_SHM_CREATE, len as u64, 0, 0, 0) };
    if r <= 0 {
        Err(())
    } else {
        Ok(r as u32)
    }
}

/// Fase 30 — `shm_map(id, hint, prot)`: mappa la regione condivisa `id` nello
/// spazio del chiamante (prot `PROT_READ`/`PROT_READ|PROT_WRITE`); le pagine
/// sono le STESSE per tutti i mappatori (scritture visibili). `hint == 0` =
/// scelta kernel. Ritorna la base o `Err`.
#[inline]
pub fn shm_map(id: u32, hint: usize, prot: u64) -> Result<usize, ()> {
    let r = unsafe { syscall4(SYS_SHM_MAP, id as u64, hint as u64, prot, 0) };
    if r < 0 {
        Err(())
    } else {
        Ok(r as usize)
    }
}


/// Termina il processo corrente con il codice `code`. Non ritorna.
#[inline]
pub fn exit(code: i64) -> ! {
    unsafe {
        syscall4(SYS_EXIT, code as u64, 0, 0, 0);
    }
    unsafe {
        core::arch::asm!("ud2", options(noreturn));
    }
}

/// Fase 14 — `kill(pid, code)`: chiede al kernel di terminare il processo
/// user `pid` con il codice `code` (cleanup differito + cascata sulla
/// discendenza + notifica `EXIT_NOTIFY` al parent). `Ok` se il processo e'
/// stato terminato, `Err` se il pid non esiste / non e' killabile (init,
/// processi kernel, se stesso).
#[inline]
pub fn kill(pid: i64, code: i64) -> Result<(), ()> {
    let r = unsafe { syscall4(SYS_KILL, pid as u64, code as u64, 0, 0) };
    if r < 0 {
        Err(())
    } else {
        Ok(())
    }
}

/// Fase 14 — `is_exit_notify(m)`: true se `m` e' la notifica kernel→parent
/// della morte di un figlio (`EXIT_NOTIFY`: w0 = exit code, w1 = pid del
/// figlio). I loop `recv` dei server/test devono ignorarla o gestirla.
#[inline]
pub fn is_exit_notify(m: &IpcMsg) -> bool {
    m.tag == EXIT_NOTIFY
}

/// Fase 19.1 — entry `ps`: snapshot di un processo (syscall 37, layout dei
/// campi in `syscall-numbers::SYS_PS_INFO`). `name` = byte del nome (max 16,
/// stop al primo NUL), `parent` = pid del padre (`None` per init/idle).
#[derive(Clone, Copy, Debug)]
pub struct PsEntry {
    pub pid: u32,
    pub name: [u8; 16],
    pub state: u8,
    pub prio: u8,
    pub parent: Option<u32>,
    pub ipc: u8,
    pub ticks: u64,
}

impl PsEntry {
    /// Lunghezza del nome (stop al primo NUL).
    pub fn name_len(&self) -> usize {
        self.name.iter().position(|&b| b == 0).unwrap_or(16)
    }
    /// Nome come `&str` ("?" se non UTF-8, mai in pratica: nomi statici).
    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len()]).unwrap_or("?")
    }
}

/// `ps_info(pid)`: snapshot del processo `pid`, `None` se lo slot e' vuoto o
/// il processo e' terminato (come `ps` salta i PID morti).
pub fn ps_info(pid: u32) -> Option<PsEntry> {
    let (rax, rdi, rsi, rdx, r10) =
        unsafe { syscall4_out(SYS_PS_INFO, pid as u64, 0, 0, 0) };
    if rax != 0 {
        return None;
    }
    let mut name = [0u8; 16];
    name[..8].copy_from_slice(&rdi.to_le_bytes());
    name[8..].copy_from_slice(&rsi.to_le_bytes());
    let parent_raw = ((rdx >> 16) & 0xFF) as u32;
    Some(PsEntry {
        pid,
        name,
        state: (rdx & 0xFF) as u8,
        prio: ((rdx >> 8) & 0xFF) as u8,
        parent: if parent_raw == 0 { None } else { Some(parent_raw - 1) },
        ipc: ((rdx >> 24) & 0xFF) as u8,
        ticks: r10,
    })
}

/// Scrive una stringa su stdout (fd 1) bypassando il line buffer.
/// Usare solo per dati binari/raw; per output di testo usare `print!`/`println!`.
#[inline]
pub fn write_stdout(buf: *const u8, count: usize) -> i64 {
    write(1, buf, count)
}

/// Scrive byte su stdout (fd 1) bypassando il line buffer.
/// Compat: usato dai programmi userspace esistenti.
#[inline]
pub fn print_string(s: &[u8]) -> i64 {
    write(1, s.as_ptr(), s.len())
}

/// `map_in(chan, phys, virt, count)`: mappa `count` pagine fisiche a partire
/// da `phys` all'indirizzo virtuale `virt` nello spazio del PEER del canale
/// `chan` (ADR-0008). Usato da userfs per iniettare la response ring del client
/// in un driver remoto (devfs/console).
#[inline]
pub fn map_in(chan: u64, phys: u64, virt: u64, count: usize) -> Result<(), ()> {
    let r = unsafe { syscall4(SYS_MAP_IN, chan, phys, virt, count as u64) };
    if r < 0 {
        Err(())
    } else {
        Ok(())
    }
}

// ── CBS bandwidth reservation (Fase 11.4) ──────────────────────────────

/// Informazioni di un server CBS.
pub struct CbsInfo {
    pub budget: u32,
    pub period: u32,
    pub remaining: i32,
}

/// Crea un server CBS con budget Q e periodo P (in tick, 1 tick = 10 ms).
/// Admission control: ritorna l'id del server o `Err(())` se la bandwidth
/// totale supererebbe il cap (~70%).
#[inline]
pub fn cbs_create(budget_ticks: u32, period_ticks: u32) -> Result<i64, ()> {
    let r = unsafe { syscall4(SYS_CBS_CREATE, budget_ticks as u64, period_ticks as u64, 0, 0) };
    if r < 0 { Err(()) } else { Ok(r) }
}

/// Lega il server CBS `server_id` al processo corrente.
#[inline]
pub fn cbs_attach(server_id: i64) -> Result<(), ()> {
    let r = unsafe { syscall4(SYS_CBS_ATTACH, server_id as u64, 0, 0, 0) };
    if r < 0 { Err(()) } else { Ok(()) }
}

/// Ritorna le informazioni di un server CBS (budget/period/remaining).
#[inline]
pub fn cbs_get_info(server_id: i64) -> Option<CbsInfo> {
    let (rax, rdi, rsi, _, _) = unsafe { syscall4_out(SYS_CBS_GET_INFO, server_id as u64, 0, 0, 0) };
    if rax < 0 {
        None
    } else {
        Some(CbsInfo {
            budget: rax as u32,
            period: rdi as u32,
            remaining: rsi as i32,
        })
    }
}
