// Split from syscall.rs (byte-identical move; see facade).
use super::dispatch::syscall_handler;

use core::ptr::{addr_of, addr_of_mut};
use x86_64::registers::model_specific::{
    Efer, EferFlags, KernelGsBase, LStar, SFMask, Star,
};
use x86_64::registers::rflags::RFlags;
use x86_64::VirtAddr;
/// Area per-core: stato del processo corrente + registri temporanei usati
/// dall'entry syscall. Gli offset sono bloccati (vedi `syscall_entry`).
#[repr(C)]
pub(super) struct PerCpu {
    pub(super) current_id: u64,  // 0x00 id del processo in esecuzione
    pub(super) rsp0: u64,        // 0x08 top dello stack kernel del processo (per RSP0)
    pub(super) current_cr3: u64, // 0x10 CR3 del processo corrente
    pub(super) user_rsp: u64,    // 0x18 RSP user salvato dall'entry (ripristinato a sysret)
    pub(super) number: u64,      // 0x20 numero di syscall
    pub(super) arg1: u64,        // 0x28
    pub(super) arg2: u64,        // 0x30
    pub(super) arg3: u64,        // 0x38
    pub(super) arg4: u64,        // 0x40
    pub(super) ipc_override: u64, // 0x48 !=0 → a sysret si sovrascrivono i registri user
                       //            con i valori ret_* (per IPC multi-register)
    pub(super) ret_rdi: u64,      // 0x50
    pub(super) ret_rsi: u64,      // 0x58
    pub(super) ret_rdx: u64,      // 0x60
    pub(super) ret_r10: u64,      // 0x68
    pub(super) user_r12_save: u64, // 0x70 staging transitorio dell'r12 user nell'entry
                        //     (copiato subito sullo stack kernel, mai letto
                        //     dopo un context switch)
}

/// Unica area per-core (single core). Vi si accede solo tramite puntatori
/// grezzi e/o via `GS.base` (`KernelGsBase`), mai per riferimento.
pub(super) static mut PERCPU: PerCpu = PerCpu {
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
