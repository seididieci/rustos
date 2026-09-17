//! GDT definitiva del kernel + TSS per-processo con I/O permission bitmap.
//!
//! Ogni processo possiede un proprio `TaskStateSegment` (ADR-0006) dentro un
//! pool statico: contiene `RSP0` (stack ring 0 su interrupt da ring 3), la IST
//! del double fault e l'I/O permission bitmap (quali porte I/O il processo puo'
//! usare a ring 3). Il context switch carica il TSS del processo tramite `ltr`
//! (`load_process_tss`); `RSP0` viene impostato una sola volta alla creazione
//! (ogni processo ha il suo TSS, non serve piu' aggiornarlo a ogni switch).
//!
//! La bitmap e' per-processo: default tutte le porte bloccate (`0xFF`); i
//! driver userspace (es. `userdisk` per ATA 0x1F0-0x1F7) abilitano solo le loro
//! porte tramite `io_ranges` alla creazione. Nessun IOPL ne' CLI/STI concesso.

use core::arch::asm;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Lazy;
use spin::Mutex;
use x86_64::VirtAddr;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;

/// Slot IST usato dal double fault handler.
pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

/// Numero di TSS per-processo nel pool. Slot 0 riservato al TSS di boot/kernel.
pub const MAX_TSS_SLOTS: usize = 32;

/// Capacita' della GDT: null + 4 selettori base + MAX_TSS_SLOTS descriptor TSS
/// (ogni system segment occupa 2 entry nel crate).
const GDT_CAPACITY: usize = 1 + 4 + MAX_TSS_SLOTS * 2;

/// Lunghezza della I/O permission bitmap: 65536 bit (8 KiB) + byte finale 0xFF.
const IO_BITMAP_LEN: usize = 8193;

const DF_STACK_SIZE: usize = 4096 * 5;

/// Stack per il double fault. `static mut` ma vi si accede solo tramite
/// puntatori grezzi (`&raw`), mai per riferimento.
static mut DF_STACK: [u8; DF_STACK_SIZE] = [0; DF_STACK_SIZE];

/// TSS + I/O bitmap contigua: `iomap_base` punta a `iomap` (subito dopo il
/// TSS, offset = size_of::<TaskStateSegment>(), gia' impostato da `new()`).
#[repr(C)]
struct TssWithIomap {
    tss: TaskStateSegment,
    iomap: [u8; IO_BITMAP_LEN],
}

const fn tss_with_iomap() -> TssWithIomap {
    TssWithIomap {
        tss: TaskStateSegment::new(),
        iomap: [0xFF; IO_BITMAP_LEN],
    }
}

/// Pool dei TSS per-processo. `MaybeUninit`: gli indirizzi sono statici (i
/// descriptor GDT sono pre-costruibili) ma i contenuti vengono inizializzati
/// al primo accesso alla GDT (closure `Lazy`), prima di qualunque processo.
static mut TSS_POOL: MaybeUninit<[TssWithIomap; MAX_TSS_SLOTS]> = MaybeUninit::uninit();

/// Prossimo slot libero: 0 = boot/kernel TSS, i processi partono da 1.
/// Fase 14 (ADR-0010): allocatore RIUUSABILE — `TSS_FREE[slot]` = true quando
/// lo slot puo' essere riallocato a un nuovo processo (basi GDT statiche, il
/// contenuto viene riconfigurato da `configure_tss` a ogni uso). Slot 0 resta
/// riservato al TSS di boot. Inizializzato in `init`.
static TSS_FREE: Mutex<[bool; MAX_TSS_SLOTS]> = Mutex::new([false; MAX_TSS_SLOTS]);

/// Base della GDT caricata (letta con `sgdt` in `init`): usata per azzerare il
/// bit "busy" dei descriptor TSS prima di ogni `ltr`.
static GDT_BASE: AtomicUsize = AtomicUsize::new(0);

fn pool_ref() -> &'static [TssWithIomap; MAX_TSS_SLOTS] {
    unsafe { (*core::ptr::addr_of!(TSS_POOL)).assume_init_ref() }
}

fn pool_mut() -> &'static mut [TssWithIomap; MAX_TSS_SLOTS] {
    unsafe { (*core::ptr::addr_of_mut!(TSS_POOL)).assume_init_mut() }
}

pub struct Selectors {
    pub code: SegmentSelector,
    pub data: SegmentSelector,
    /// Selettori user mode (DPL 3): usati per costruire i frame CPU RPL3 (CS/SS
    /// user) dei processi utente (Fase 6.2).
    pub user_code: SegmentSelector,
    pub user_data: SegmentSelector,
    tss_pool: [SegmentSelector; MAX_TSS_SLOTS],
}

impl Selectors {
    /// Selettore del TSS del processo nel pool (da usare con `ltr`).
    pub fn tss_selector(&self, slot: usize) -> SegmentSelector {
        self.tss_pool[slot]
    }
}

/// Selettori della GDT attiva. Usati per costruire i frame di avvio dei processi.
pub fn selectors() -> &'static Selectors {
    &GDT.1
}

static GDT: Lazy<(GlobalDescriptorTable<GDT_CAPACITY>, Selectors)> = Lazy::new(|| {
    let mut gdt = GlobalDescriptorTable::<GDT_CAPACITY>::empty();
    let code = gdt.append(Descriptor::kernel_code_segment());
    let data = gdt.append(Descriptor::kernel_data_segment());
    // Nota: user_data precede user_code. Su `sysret` la CPU carica SS=CS-8,
    // quindi il selettore data deve stare 8 byte SOTTO quello code.
    let user_data = gdt.append(Descriptor::user_data_segment());
    let user_code = gdt.append(Descriptor::user_code_segment());

    // Inizializza il pool prima di costruire i descriptor (il crate legge il
    // byte terminatore 0xFF della bitmap per validare l'I/O map).
    for slot in pool_mut().iter_mut() {
        *slot = tss_with_iomap();
    }

    // Descriptor TSS per ogni slot. `iomap_base` e' gia' l'offset di `iomap`
    // nel wrapper (size_of TSS), quindi il limite copre bitmap + terminatore.
    let mut tss_pool = [SegmentSelector(0); MAX_TSS_SLOTS];
    for i in 0..MAX_TSS_SLOTS {
        let slot: &'static TssWithIomap = &pool_ref()[i];
        let desc = Descriptor::tss_segment_with_iomap(&slot.tss, &slot.iomap)
            .expect("TSS descriptor con I/O bitmap");
        tss_pool[i] = gdt.append(desc);
    }

    (
        gdt,
        Selectors {
            code,
            data,
            user_code,
            user_data,
            tss_pool,
        },
    )
});

/// Alloca uno slot TSS per un nuovo processo. Ritorna `None` se il pool e'
/// esaurito. Gli slot liberati (`free_tss_slot`) vengono riusati.
pub fn alloc_tss_slot() -> Option<usize> {
    let mut free = TSS_FREE.lock();
    for slot in 1..MAX_TSS_SLOTS {
        if free[slot] {
            free[slot] = false;
            return Some(slot);
        }
    }
    None
}

/// Rilascia uno slot TSS (Fase 14): torna disponibile per un nuovo processo.
/// Slot 0 (boot) non va mai liberato.
pub fn free_tss_slot(slot: usize) {
    if slot >= 1 && slot < MAX_TSS_SLOTS {
        TSS_FREE.lock()[slot] = true;
    }
}

/// Abilita le porte I/O in `ranges` (inclusive) per il processo `slot`,
/// pulendo i bit corrispondenti nella sua bitmap. Non resetta il resto: per
/// un processo nuovo la bitmap e' gia' tutta `0xFF` (inizializzazione pool).
pub fn allow_ports(slot: usize, ranges: &[(u16, u16)]) {
    let tss = &mut pool_mut()[slot];
    for &(lo, hi) in ranges {
        for port in lo..=hi {
            let byte = (port as usize) / 8;
            let bit = (port as usize) % 8;
            tss.iomap[byte] &= !(1 << bit);
        }
    }
}

/// Configura il TSS del processo `slot`: RSP0 (stack kernel), IST double
/// fault e I/O bitmap per `io_ranges`. Da chiamare una volta alla creazione.
pub fn configure_tss(slot: usize, rsp0: VirtAddr, io_ranges: &[(u16, u16)]) {
    let tss = &mut pool_mut()[slot];
    tss.tss.privilege_stack_table[0] = rsp0;
    tss.tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] =
        VirtAddr::from_ptr(&raw const DF_STACK) + DF_STACK_SIZE as u64;
    // Reset bitmap (idempotente) poi abilita le porte richieste.
    tss.iomap.fill(0xFF);
    allow_ports(slot, io_ranges);
}

/// Carica nel task register il TSS del processo `sel`. Chiamato a ogni
/// context switch: da qui in poi la CPU usa quel TSS per RSP0 e I/O bitmap.
///
/// Il bit "busy" (bit 41 del type) viene azzerato prima del `ltr`: la CPU lo
/// setta nel descriptor in GDT al primo load e `ltr` rifiuterebbe (con #GP) un
/// TSS gia' marcato busy al reload.
pub fn load_process_tss(sel: SegmentSelector) {
    use x86_64::instructions::tables::load_tss;

    let desc_addr = GDT_BASE.load(Ordering::Relaxed) + (sel.index() as usize) * 8;
    unsafe {
        let d = desc_addr as *mut u64;
        *d &= !(1u64 << 41); // clear busy bit (bit 1 del type 0b1001)
    }

    unsafe { load_tss(sel) };
}

pub fn init() {
    use x86_64::instructions::segmentation::{DS, ES, SS, Segment};
    use x86_64::instructions::tables::load_tss;

    // Forza la GDT (inizializza il pool + descriptor), poi configura il TSS
    // di boot (slot 0): RSP0 pre-scheduler valido (DF_STACK), nessuna porta.
    let (table, sel) = &*GDT;
    configure_tss(0, VirtAddr::from_ptr(&raw const DF_STACK) + DF_STACK_SIZE as u64, &[]);

    // Pool TSS riusabile (Fase 14): tutti gli slot tranne il 0 (boot) liberi.
    {
        let mut free = TSS_FREE.lock();
        for s in 1..MAX_TSS_SLOTS {
            free[s] = true;
        }
    }

    table.load();

    unsafe {
        // Il crate non offre far-jump sicuri: ricaricare CS richiede un
        // trasferimento far. retfq con indirizzo costruito a mano.
        asm!(
            "push {sel}",
            "lea {ret}, [rip + 2f]",
            "push {ret}",
            "retfq",
            "2:",
            sel = in(reg) sel.code.0 as u64,
            ret = out(reg) _,
            options(nostack)
        );

        DS::set_reg(sel.data);
        ES::set_reg(sel.data);
        SS::set_reg(sel.data);
        load_tss(sel.tss_selector(0));
    }

    // Cache della base GDT (per azzerare il bit busy dei TSS, vd. sopra).
    let mut gdtr = [0u8; 10];
    unsafe {
        core::arch::asm!("sgdt [{}]", in(reg) gdtr.as_mut_ptr(), options(nostack));
    }
    GDT_BASE.store(
        u64::from_le_bytes([
            gdtr[2], gdtr[3], gdtr[4], gdtr[5], gdtr[6], gdtr[7], gdtr[8], gdtr[9],
        ]) as usize,
        Ordering::Relaxed,
    );

    crate::serial_println!(
        "[gdt ] caricata (TSS per-processo x{}, IST double fault, segmenti user)",
        MAX_TSS_SLOTS
    );
}
