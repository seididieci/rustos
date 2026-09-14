//! Meccanismo syscall/sysret (Fase 6.3).
//!
//! Configura i MSR `STAR`/`LSTAR`/`SFMASK` + `EFER.SCE` e `KernelGsBase`, e
//! fornisce l'entry assembly che trasferisce da ring 3 a ring 0 via `syscall`.
//!
//! A differenza di un interrupt, la CPU su `syscall` **non** cambia stack e
//! non fa push di un frame: salva solo RIP→RCX e RFLAGS→R11, poi salta a
//! `LSTAR` con `CS` dal `STAR`. L'entry deve quindi, manualmente:
//!   1. fare `swapgs` per portare `GS.base` verso l'area `PERCPU` del kernel;
//!   2. `mov rsp` verso uno stack kernel per-processo (campo `rsp0`);
//!   3. salvare RSP user (`user_rsp`) e gli argomenti su `PERCPU`;
//!   4. chiamare `syscall_handler` e, al ritorno, ripristinare RSP user,
//!      `swapgs` di nuovo e `sysretq`.
//!
//! `PERCPU` e' un'area per-core: oggi ce n'e' una sola (single core), ma il
//! layout `#[repr(C)]` con offset fissi e `KernelGsBase` per-core e' pensato
//! per passare al multicore senza ristrutturare l'entry.

use core::ptr::{addr_of, addr_of_mut};
use x86_64::registers::model_specific::{
    Efer, EferFlags, KernelGsBase, LStar, SFMask, Star,
};
use x86_64::registers::rflags::RFlags;
use x86_64::VirtAddr;

/// Area per-core: stato del processo corrente + registri temporanei usati
/// dall'entry syscall. Gli offset sono bloccati (vedi `syscall_entry`).
#[repr(C)]
struct PerCpu {
    current_id: u64,  // 0x00 id del processo in esecuzione
    rsp0: u64,        // 0x08 top dello stack kernel del processo (per RSP0)
    current_cr3: u64, // 0x10 CR3 del processo corrente
    user_rsp: u64,    // 0x18 RSP user salvato dall'entry (ripristinato a sysret)
    number: u64,      // 0x20 numero di syscall
    arg1: u64,        // 0x28
    arg2: u64,        // 0x30
    arg3: u64,        // 0x38
    arg4: u64,        // 0x40
    ipc_override: u64, // 0x48 !=0 → a sysret si sovrascrivono i registri user
                       //            con i valori ret_* (per IPC multi-register)
    ret_rdi: u64,      // 0x50
    ret_rsi: u64,      // 0x58
    ret_rdx: u64,      // 0x60
    ret_r10: u64,      // 0x68
    user_r12_save: u64, // 0x70 staging transitorio dell'r12 user nell'entry
                        //     (copiato subito sullo stack kernel, mai letto
                        //     dopo un context switch)
}

/// Unica area per-core (single core). Vi si accede solo tramite puntatori
/// grezzi e/o via `GS.base` (`KernelGsBase`), mai per riferimento.
static mut PERCPU: PerCpu = PerCpu {
    current_id: 0,
    rsp0: 0,
    current_cr3: 0,
    user_rsp: 0,
    number: 0,
    arg1: 0,
    arg2: 0,
    arg3: 0,
    arg4: 0,
    ipc_override: 0,
    ret_rdi: 0,
    ret_rsi: 0,
    ret_rdx: 0,
    ret_r10: 0,
    user_r12_save: 0,
};

/// Entry assembly della syscall: punto d'ingresso di `LSTAR`.
///
/// A questo punto `GS.base` e' quello dell'utente (oppure 0); con `swapgs`
/// diventa `PERCPU` (kernel). Poi si passa allo stack kernel per-processo.
/// Deve essere `naked`: un prologue sposterebbe RSP (che e' ancora quello user).
///
/// # Safety
/// Solo `LSTAR` deve trasferire qui.
#[unsafe(naked)]
pub unsafe extern "C" fn syscall_entry() -> ! {
    // r12 = base di PERCPU (callee-saved: preservato da `syscall_handler`).
    // GS.base dopo swapgs punta a PERCPU (per il futuro multicore per-core).
    //
    // Layout dello stack kernel per-processo (dal fondo, 1° push = piu' in
    // basso):
    //   [user_rsp] [user_r12] [r8 r9 r10 rdi rsi rdx rcx r11]
    // user_rsp e user_r12 sono salvati QUI (non in PERCPU) perche' PERCPU e'
    // condiviso: in una syscall che blocca (recv/send), un altro processo puo'
    // sovrascrivere user_rsp prima che il processo venga ripreso → sysret
    // tornerebbe con uno stack corrotto. Lo stack kernel e' per-processo e
    // resta valido attraverso il context switch.
    //
    // L'`r12` user (callee-saved) e' staggiato prima in PERCPU.user_r12_save
    // (offset 0x70, transitorio: copiato sullo stack kernel appena sotto,
    // PRIMA di qualsiasi context switch) perche' serve `r12` come base PERCPU.
    // MAI scrivere sullo stack user nell'entry: corromperebbe la red zone
    // (128 byte sotto RSP) che il compilatore user assume intatta.
    core::arch::naked_asm!(
        // Niente swapgs: PERCPU e' uno static raggiungibile rip-relative, e su
        // un kernel single-CPU non serve GS come base per-cpu. Lo swapgs era
        // la fonte di un bug subdolo: lo stato GS.base/KernelGsBase e' globale
        // per la CPU, ma una syscall che BLOCCA (send/recv) lasciava lo stato
        // "swapped" attraverso il context switch; il conteggio degli swapgs
        // per-CPU divergeva da quello per-processo → GS.base=0 nel handler →
        // `mov gs:0x18, rsp` scriveva nel vuoto → user_rsp stale → sysret con
        // stack sbagliato → salto a rip=0.
        //
        // r12 user deve essere preservato (callee-saved): staging su PERCPU,
        // poi copia sullo stack kernel subito dopo lo switch.
        "mov qword ptr [rip + {p}+0x18], rsp", // PerCpu.user_rsp (transitorio)
        "mov qword ptr [rip + {p}+0x70], r12", // PerCpu.user_r12_save (transitorio)
        "lea r12, [rip + {p}]",
        // salva numero syscall + argomenti
        "mov [r12 + 0x20], rax", // number
        "mov [r12 + 0x28], rdi", // arg1
        "mov [r12 + 0x30], rsi", // arg2
        "mov [r12 + 0x38], rdx", // arg3
        "mov [r12 + 0x40], r10", // arg4
        // passa allo stack kernel per-processo
        "mov rsp, [r12 + 0x08]", // PerCpu.rsp0
        // sposta user_rsp e user_r12 sullo stack kernel (1° e 2° push)
        "push qword ptr [r12 + 0x18]", // user_rsp
        "push qword ptr [r12 + 0x70]", // user_r12
        // ABI syscall: l'utente si aspetta TUTTI i registri preservati tranne
        // RAX (valore di ritorno) e RCX/R11 (sovrascritti da syscall/sysret).
        // syscall_handler e' una funzione C: il compilatore clobbera r8-r11 e
        // gli argomenti caller-saved. Senza salvarli, un processo che tiene un
        // valore in r8/r9/r10 attraverso una syscall (es. il pid in r8 dopo
        // getpid) leggerebbe spazzatura al ritorno → crash. Salviamo tutto.
        "push r8",
        "push r9",
        "push r10",
        "push rdi",
        "push rsi",
        "push rdx",
        "push rcx",
        "push r11",
        // dispatch (il risultato resta in rax)
        "call {handler}",
        "pop r11",
        "pop rcx",
        "pop rdx",
        "pop rsi",
        "pop rdi",
        "pop r10",
        "pop r9",
        "pop r8",
        // IPC multi-register: se il handler ha impostato ipc_override, svuota
        // i registri user rdi/rsi/rdx/r10/r8 con i valori di ritorno ret_*.
        "cmp qword ptr [r12 + 0x48], 0",
        "je 2f",
        "mov rdi, [r12 + 0x50]",
        "mov rsi, [r12 + 0x58]",
        "mov rdx, [r12 + 0x60]",
        "mov r10, [r12 + 0x68]",
        "2:",
        // ripristina user_r12 e user_rsp dallo stack kernel (r12 non serve piu'
        // come base): i due valori salvati come primo push in fondo all'area.
        "pop r12",
        "pop rsp",
        "sysretq",
        p = sym PERCPU,
        handler = sym syscall_handler,
    );
}

/// Configura i MSR per la syscall e punta `KernelGsBase` a `PERCPU`.
pub fn init() {
    use x86_64::structures::gdt::SegmentSelector;

    let sel = crate::gdt::selectors();

    // STAR:
    //  - syscall (ring 3→0): CS = kernel code, SS = CS+8 = kernel data.
    //  - sysret (ring 0→3): CS = |user_code, SS = |user_data; la CPU impone
    //    SS = CS−8, quindi il selettore user_data deve stare SOTTO user_code.
    //    (vedi gdt::init per l'ordine delle entry).
    let cs_sysret = SegmentSelector(sel.user_code.0 | 0x3);
    let ss_sysret = SegmentSelector(sel.user_data.0 | 0x3);
    Star::write(cs_sysret, ss_sysret, sel.code, sel.data)
        .expect("STAR: segmenti syscall non coerenti");

    // SFMASK: maschera IF (e TF/DF/altri) durante la syscall → niente interrupt
    // nel tratto critico GS/RSP.
    SFMask::write(RFlags::from_bits_truncate(0x3F7));

    // EFER.SCE: abilita le istruzioni syscall/sysret.
    unsafe {
        Efer::write(Efer::read() | EferFlags::SYSTEM_CALL_EXTENSIONS);
    }

    // LSTAR → entry assembly.
    LStar::write(VirtAddr::new(syscall_entry as *const () as usize as u64));

    // KernelGsBase → area per-core; GS.base utente resta a 0 (swapgs alterna).
    KernelGsBase::write(VirtAddr::new(addr_of!(PERCPU) as u64));

    crate::serial_println!(
        "[syscall] syscall/sysret abilitate (STAR/LSTAR/SFMASK + EFER.SCE)"
    );
}

/// Aggiorna lo stato del processo corrente su `PERCPU`. Chiamato dal context
/// switch: cosi' l'entry syscall trova id/rsp0/cr3 del processo in esecuzione.
pub fn set_current(id: usize, rsp0: u64, cr3: u64) {
    unsafe {
        let p = addr_of_mut!(PERCPU);
        (*p).current_id = id as u64;
        (*p).rsp0 = rsp0;
        (*p).current_cr3 = cr3;
    }
}

/// Id del processo corrente (per `getpid`).
pub fn current_id() -> u64 {
    unsafe { (*(addr_of!(PERCPU))).current_id }
}

/// Handler di dispatch: legge gli argomenti riempiti dall'entry e chiama la
/// syscall richiesta. Firmato `extern "C" fn() -> i64` per essere invocabile
/// dall'assembly; il risultato torna in RAX a `sysretq`.
#[unsafe(no_mangle)]
extern "C" fn syscall_handler() -> i64 {
    unsafe {
        let p = addr_of_mut!(PERCPU);
        // Reset del flag di ritorno multi-register: di default restituiamo i
        // registri user preservati (solo RAX cambia). Le syscall IPC lo
        // impostano per svuotare rdi/rsi/rdx/r10 con i valori di risposta.
        (*p).ipc_override = 0;
        match (*p).number {
            syscall_numbers::SYS_EXIT => sys_exit((*p).arg1 as i64),
            syscall_numbers::SYS_WRITE => sys_write((*p).arg1, (*p).arg2 as *const u8, (*p).arg3 as usize),
            syscall_numbers::SYS_GETPID => sys_getpid(),
            syscall_numbers::SYS_SEND => sys_send((*p).arg1 as usize, (*p).arg2, (*p).arg3, (*p).arg4),
            syscall_numbers::SYS_SEND_ASYNC => sys_send_async((*p).arg1 as usize, (*p).arg2, (*p).arg3, (*p).arg4),
            syscall_numbers::SYS_RECV => sys_recv(),
            syscall_numbers::SYS_RECV_NONBLOCK => sys_recv_nonblock(),
            syscall_numbers::SYS_REPLY => sys_reply((*p).arg1, (*p).arg2, (*p).arg3),
            syscall_numbers::SYS_SERVICE_REGISTER => sys_service_register((*p).arg1),
            syscall_numbers::SYS_SERVICE_LOOKUP => sys_service_lookup((*p).arg1),
            syscall_numbers::SYS_SPAWN => sys_spawn((*p).arg1, (*p).arg2 as usize),
            syscall_numbers::SYS_MAP_PHYSICAL => sys_map_physical((*p).arg1, (*p).arg2, (*p).arg3 as usize),
            syscall_numbers::SYS_GET_TICKS => sys_get_ticks(),
            syscall_numbers::SYS_SBRK => sys_sbrk((*p).arg1),
            // Ring buffer SPSC per-processo (Fase 10.2).
            syscall_numbers::SYS_RING_ALLOC => sys_ring_alloc(),
            syscall_numbers::SYS_MAP_IN => sys_map_in((*p).arg1 as usize, (*p).arg2, (*p).arg3, (*p).arg4 as usize),
            // CBS bandwidth reservation (Fase 11.4).
            syscall_numbers::SYS_CBS_CREATE => sys_cbs_create((*p).arg1, (*p).arg2),
            syscall_numbers::SYS_CBS_ATTACH => sys_cbs_attach(),
            syscall_numbers::SYS_CBS_GET_INFO => sys_cbs_get_info((*p).arg1),
            // Fase 14 (ADR-0010): kill di un processo user.
            syscall_numbers::SYS_KILL => sys_kill((*p).arg1, (*p).arg2 as i64),
            // Fase 14 (init-restart): pid dell'owner di un servizio.
            syscall_numbers::SYS_SERVICE_PID => sys_service_pid((*p).arg1),
            _ => -1,
        }
    }
}

/// Applica il risultato di una primitiva IPC ai registri di ritorno della
/// syscall: imposta i valori di ritorno e il flag `ipc_override` perche'
/// l'entry riempia rdi/rsi/rdx/r10. Ritorna il valore di `rax` (stato).
fn apply_ipc(r: crate::sched::IpcResult) -> i64 {
    unsafe {
        let p = addr_of_mut!(PERCPU);
        (*p).ipc_override = 1;
        (*p).ret_rdi = r.rdi;
        (*p).ret_rsi = r.rsi;
        (*p).ret_rdx = r.rdx;
        (*p).ret_r10 = r.r10;
    }
    r.rax
}

/// ADR-0008 — `send(channel, tag, w0, w1)`: invia su canale (0 = canale di
/// nascita verso il parent). Ritorna la reply (tag,w0,w1) in rsi/rdx/r10.
fn sys_send(channel: usize, tag: u64, w0: u64, w1: u64) -> i64 {
    apply_ipc(crate::sched::ipc_send(channel, tag, w0, w1))
}

/// Fase 13 — `send_async(channel, tag, w0, w1)`: come send ma NON blocca il
/// mittente. Ritorna il req_id (>= 1) in rax, o -1 se coda piena / canale
/// morto. Solo il valore di rax e' significativo (nessun ipc_override).
fn sys_send_async(channel: usize, tag: u64, w0: u64, w1: u64) -> i64 {
    crate::sched::ipc_send_async(channel, tag, w0, w1).rax
}

/// ADR-0008 — `recv()`: riceve il prossimo messaggio.
/// Ritorna (channel, tag, w0, w1) in rdi/rsi/rdx/r10.
fn sys_recv() -> i64 {
    apply_ipc(crate::sched::ipc_recv())
}

/// Fase 13 — `recv_nonblock()`: come recv ma ritorna -1 subito se la coda e'
/// vuota (nessun blocco). Ritorna (req_id o channel, tag, w0, w1).
fn sys_recv_nonblock() -> i64 {
    apply_ipc(crate::sched::ipc_recv_nonblock())
}

/// ADR-0008 — `reply(tag, w0, w1)`: risponde al mittente del messaggio che il
/// chiamante sta elaborando (canale impostato da recv).
fn sys_reply(tag: u64, w0: u64, w1: u64) -> i64 {
    apply_ipc(crate::sched::ipc_reply(tag, w0, w1))
}

/// exit(code): termina il processo corrente. Non ritorna (tipo `!` → i64).
fn sys_exit(code: i64) -> i64 {
    crate::sched::exit_current(code)
}

/// Fase 14 — `kill(pid, code)`: termina un processo user per la stessa via di
/// exit (cleanup differito + cascata sulla discendenza + notifica al parent).
/// Ritorna 0 se il processo e' stato terminato, -1 se il pid non esiste / non
/// e' killabile (init, processi kernel, se stesso).
fn sys_kill(pid: u64, code: i64) -> i64 {
    if crate::sched::kill(pid as usize, code) {
        0
    } else {
        -1
    }
}

/// getpid(): id del processo corrente.
fn sys_getpid() -> i64 {
    current_id() as i64
}

/// ADR-0008 — `service_register(service)`: il chiamante occupa lo slot del
/// servizio `service`. -1 se gia' occupato da un processo vivo.
fn sys_service_register(service_disc: u64) -> i64 {
    let service = match service_from_disc(service_disc) {
        Some(s) => s,
        None => return -1,
    };
    match crate::channels::register(service, current_id() as usize) {
        Ok(()) => {
            crate::serial_println!(
                "[svc] '{}' registrato da pid={}",
                service_name(service), current_id()
            );
            0
        }
        Err(()) => -1,
    }
}

/// ADR-0008 — `service_lookup(service)`: risolve il servizio in un channel
/// verso l'attuale owner. Ritorna il channel id, o -1 se non registrato.
fn sys_service_lookup(service_disc: u64) -> i64 {
    let service = match service_from_disc(service_disc) {
        Some(s) => s,
        None => return -1,
    };
    let me = current_id() as usize;
    match crate::channels::lookup(service) {
        Some(owner) => match crate::channels::alloc(me, owner) {
            Some(chan) => chan as i64,
            None => -1,
        },
        None => -1,
    }
}

/// Fase 14 (init-restart) — `service_pid(service)`: ritorna il pid
/// dell'attuale owner del servizio, o -1 se non registrato. Nota: come
/// `lookup`, si fida dello slot owner (azzerato da `release_pid` alla morte;
/// riuso PID da parte di terzi nel mentre = futura generazione, vedi ADR-0010).
fn sys_service_pid(service_disc: u64) -> i64 {
    let service = match service_from_disc(service_disc) {
        Some(s) => s,
        None => return -1,
    };
    match crate::channels::lookup(service) {
        Some(owner) => owner as i64,
        None => -1,
    }
}

/// Converti un discriminant in un `Service` valido.
fn service_from_disc(disc: u64) -> Option<syscall_numbers::Service> {
    if disc < syscall_numbers::SERVICE_COUNT as u64 {
        Some(unsafe { core::mem::transmute(disc) })
    } else {
        None
    }
}

/// Nome leggibile di un servizio (per log di debug).
fn service_name(s: syscall_numbers::Service) -> &'static str {
    match s {
        syscall_numbers::Service::Console => "console",
        syscall_numbers::Service::Fs => "fs",
        syscall_numbers::Service::Devfs => "devfs",
        syscall_numbers::Service::Init => "init",
        syscall_numbers::Service::Test => "test",
        syscall_numbers::Service::Kbd => "kbd",
        syscall_numbers::Service::Tty => "tty",
        syscall_numbers::Service::Disk => "disk",
    }
}

/// spawn(name_ptr, name_len): crea un nuovo processo dal binario embedded
/// chiamato `name`. Crea il canale di nascita tra il chiamante (parent) e il
/// figlio (ADR-0008): il figlio lo eredita come canale 0, e il chiamante riceve
/// qui il channel id. Ritorna il channel id, o -1 se il nome non e' noto / la
/// creazione fallisce.
fn sys_spawn(name_ptr: u64, name_len: usize) -> i64 {
    if name_len == 0 || name_len > 64 {
        return -1;
    }
    // Validazione: il nome deve stare nello spazio user mappato (U=1).
    if !crate::vmm_user::is_user_range(name_ptr, name_len) {
        crate::serial_println!("[syscall] spawn: nome fuori dallo spazio user");
        return -1;
    }
    let bytes =
        unsafe { core::slice::from_raw_parts(name_ptr as *const u8, name_len) };
    let name = match core::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let parent = current_id() as usize;
    match crate::user_binary::spawn_named(name, Some(parent), None) {
        Some(pid) => {
            // Canale di nascita tra parent e figlio: il figlio lo usera' come
            // canale 0 (parent), il parent come handle di ritorno di spawn.
            // Mai panic qui (Fase 15): a pool esaurito ritorna -1 e init
            // riporta il fallimento (un panic nel kernel inchioderebbe il boot).
            match crate::channels::alloc(parent, pid) {
                Some(chan) => {
                    crate::sched::set_parent_chan(pid, Some(chan));
                    crate::serial_println!("[spawn] '{}' → pid={} canale={}", name, pid, chan);
                    chan as i64
                }
                None => {
                    crate::serial_println!("[syscall] spawn: pool canali esaurito");
                    -1
                }
            }
        }
        None => {
            crate::serial_println!("[syscall] spawn: binario sconosciuto '{}'", name);
            -1
        }
    }
}

/// write(fd, buf, count): stampa su seriale per fd 1/2; ogni altro fd
/// (nessun file implementato in questa fase) → -1.
fn sys_write(fd: u64, buf: *const u8, count: usize) -> i64 {
    if fd != 1 && fd != 2 {
        return -1;
    }
    if count == 0 {
        return 0;
    }
    // Validazione: il buffer deve stare nel range user mappato (U=1).
    if !crate::vmm_user::is_user_range(buf as u64, count) {
        crate::serial_println!("[syscall] write: puntatore fuori dallo spazio user");
        return -1;
    }
    let slice = unsafe { core::slice::from_raw_parts(buf, count) };
    let s = alloc::string::String::from_utf8_lossy(slice);
    crate::serial_println!("{}", s);
    count as i64
}

/// map_physical(phys_addr, virt_addr, count): mappa `count` pagine fisiche
/// a partire da `phys_addr` all'indirizzo virtuale `virt_addr` nello spazio
/// del chiamante. Usato dal console server (VGA), da userfs (finestra FS di
/// un client) e dalla test suite (pagina scratch MAP_TEST_PHYS).
fn sys_map_physical(phys_addr: u64, virt_addr: u64, count: usize) -> i64 {
    const PAGE_SIZE: u64 = 0x1000;
    const MAX_PAGES: usize = 256;

    if phys_addr & (PAGE_SIZE - 1) != 0 {
        return -1; // phys_addr non allineato a pagina
    }
    if virt_addr < crate::vmm_user::USER_BASE {
        return -1; // virt_addr fuori spazio user
    }
    if count == 0 || count > MAX_PAGES {
        return -1;
    }

    let cr3 = unsafe { (*(addr_of!(PERCPU))).current_cr3 };
    if cr3 == 0 {
        return -1; // cr3 non impostata
    }
    unsafe {
        crate::vmm_user::map_user_region(cr3, virt_addr, phys_addr, count);
    }
    // La PTE puo' gia' esistere (es. userfs rimappa la finestra FS a ogni
    // client): invalida la TLB perche' il processo continua a girare dopo la
    // syscall e non deve riusare la traduzione vecchia.
    for i in 0..count {
        crate::vmm_user::flush_page(virt_addr + (i as u64) * PAGE_SIZE);
    }
    0
}

/// get_ticks(): ritorna il contatore corrente di PIT ticks (100 Hz).
fn sys_get_ticks() -> i64 {
    crate::pit::ticks() as i64
}

/// sys_sbrk(inc): estende (solo crescita) l'heap del processo corrente di `inc`
/// byte, arrotondati alla pagina. NON mappa nulla: riserva solo VA aggiornando
/// il `heap_brk`. Le pagine sotto il break vengono materializzate lazy dal
/// page-fault handler (demand-zero) al primo accesso.
/// Ritorna il vecchio `heap_brk` (inizio della nuova regione) o -1 se
/// l'estensione non e' possibile (overflow / oltre il tetto soft).
fn sys_sbrk(inc: u64) -> i64 {
    const PAGE: u64 = 0x1000;
    let cur = current_id() as usize;
    let old = crate::vmm_user::heap_brk(cur);
    if inc == 0 {
        return old as i64;
    }
    // Arrotonda a pagina (saturando: inc enormi falliscono dopo).
    let n = inc.saturating_add(PAGE - 1) & !(PAGE - 1);
    let new = match old.checked_add(n) {
        Some(v) => v,
        None => return -1,
    };
    if new > crate::vmm_user::USER_HEAP_LIMIT {
        return -1;
    }

    crate::vmm_user::set_heap_brk(cur, new);
    old as i64
}

// ── Ring buffer SPSC per-processo (Fase 10.2) ─────────────────────
//
// Ogni processo ha una Coppia di pagine ring (request + response) allocata
// dalla syscall `SYS_RING_ALLOC` e mappata a `USER_FS_BUFFER` (request) e
// `USER_FS_BUFFER+0x1000` (response). Il chiamante registra entrambi gli
// indirizzi fisici presso userfs con una IPC `FS_BUF_REG`. Le operazioni FS
// sono IPC dirette client→userfs con trasferimento dati via ring buffer.

/// ring_alloc(): alloca due pagine ring (request + response) per il processo
/// corrente, le mappa a `USER_FS_BUFFER` e `USER_FS_BUFFER+0x1000`, e
/// ritorna gli indirizzi fisici via IpcResult (req_phys in rax, resp_phys
/// in rdi). -1 su OOM o errore.
/// Ogni chiamata da' pagine FRESCHE (Fase 16: un processo puo' allocare piu'
/// coppie, es. userdisk FS+DISK). Il mapping e' NON-owned: il free avviene via
/// record a teardown (`free_ring_pages`), mai double-free col walk owned.
fn sys_ring_alloc() -> i64 {
    let cur = current_id() as usize;
    let (req_phys, resp_phys) = match crate::vmm_user::alloc_ring_pages(cur) {
        Some(p) => p,
        None => {
            crate::serial_println!("[syscall] ring_alloc: oom");
            return -1;
        }
    };
    let cr3 = unsafe { (*(addr_of!(PERCPU))).current_cr3 };
    if cr3 == 0 {
        return -1;
    }
    unsafe {
        crate::vmm_user::map_user_region(cr3, crate::vmm_user::USER_FS_BUFFER, req_phys, 1);
        crate::vmm_user::map_user_region(cr3, crate::vmm_user::USER_RESP_RING, resp_phys, 1);
    }
    crate::vmm_user::flush_page(crate::vmm_user::USER_FS_BUFFER);
    crate::vmm_user::flush_page(crate::vmm_user::USER_RESP_RING);
    // Restituiamo entrambi gli indirizzi fisici via IpcResult.
    apply_ipc(crate::sched::IpcResult { rax: req_phys as i64, rdi: resp_phys, rsi: 0, rdx: 0, r10: 0 })
}

/// map_in(chan, phys, virt, count): mappa `count` pagine fisiche a partire da
/// `phys` all'indirizzo virtuale `virt` nello spazio del PEER del canale
/// `chan` (ADR-0008). Mapper generico cross-process: usato da userfs per
/// iniettare la response ring del client in un driver remoto (devfs/console).
///
/// Validazione: il peer deve essere un processo user, phys allineata a pagina,
/// virt nello spazio user, count <= 16. Ritorna 0 o -1.
fn sys_map_in(chan: usize, phys: u64, virt_addr: u64, count: usize) -> i64 {
    const PAGE_SIZE: u64 = 0x1000;
    const MAX_PAGES: usize = 16;

    if phys & (PAGE_SIZE - 1) != 0 {
        return -1;
    }
    if virt_addr < crate::vmm_user::USER_BASE {
        return -1;
    }
    if count == 0 || count > MAX_PAGES {
        return -1;
    }
    // Il target e' il peer del canale: deve essere un processo user esistente
    // (page table propria). Channel 0 = canale di nascita.
    let me = current_id() as usize;
    let real = if chan == syscall_numbers::CHANNEL_PARENT as usize {
        crate::sched::parent_channel(me)
    } else {
        Some(chan)
    };
    let target_pid = match real.and_then(|c| crate::channels::peer(c, me)) {
        Some(p) => p,
        None => return -1,
    };
    let cr3 = match crate::sched::process_cr3(target_pid) {
        Some(cr3) if cr3 != crate::vmm_user::kernel_cr3() => cr3,
        _ => {
            crate::serial_println!("[syscall] map_in: peer {} non e' un processo user", target_pid);
            return -1;
        }
    };
    unsafe {
        crate::vmm_user::map_user_region(cr3, virt_addr, phys, count);
    }
    for i in 0..count {
        crate::vmm_user::flush_page(virt_addr + (i as u64) * PAGE_SIZE);
    }
    0
}

// ── CBS syscall handlers (Fase 11.4) ────────────────────────────────

/// cbs_create(budget, period): crea un server CBS con i parametri dati.
/// Esegue l'admission control: ritorna l'id del server o -1.
fn sys_cbs_create(budget: u64, period: u64) -> i64 {
    match crate::cbs::create(budget as u32, period as u32) {
        Ok(id) => id as i64,
        Err(()) => -1,
    }
}

/// cbs_attach(): lega il server CBS indicato al processo corrente.
/// Il server_id viene passato in arg1, il PID corrente e' da `current_id`.
fn sys_cbs_attach() -> i64 {
    let server_id = unsafe { (*(addr_of!(PERCPU))).arg1 as usize };
    let pid = current_id() as usize;
    match crate::cbs::attach(server_id, pid) {
        Ok(()) => {
            if let Some(proc) = crate::sched::process_of(pid) {
                unsafe { (*proc).cbs_server = Some(server_id); }
            }
            0
        }
        Err(()) => -1,
    }
}

/// cbs_get_info(server_id): ritorna le informazioni di un server CBS.
/// Return: rax = budget, rdi = period, rsi = remaining (via ipc_override).
fn sys_cbs_get_info(server_id: u64) -> i64 {
    match crate::cbs::get_info(server_id as usize) {
        Some(info) => {
            unsafe {
                let p = addr_of_mut!(PERCPU);
                (*p).ipc_override = 1;
                (*p).ret_rdi = info.period as u64;
                (*p).ret_rsi = info.remaining as u64;
            }
            info.budget as i64
        }
        None => -1,
    }
}
