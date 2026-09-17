//! Binari utente reali (Fase 6.4 + Fase 7 + Fase 8.1): codice compilato dai
//! crate freestanding di `userland/` (servizi utente) e `testland/` (test
//! suite). I binari raw `userland/build/*.bin` e `testland/build/*.bin` sono
//! generati da `scripts/build-userland.sh` / `scripts/build-tests.sh` e inclusi
//! qui via `include_bytes!`. `spawn_named` permette di creare un processo dal
//! nome (usato dalla syscall `spawn`). A ogni spawn il binario viene COPIATO
//! in frame privati (see `copy_binary`): due istanze della stessa bin non
//! condividono `.bss`/`.data` mutabili.

/// Tipo wrapper allineato al frame (4096): una `static` non puo' avere
/// `repr(align)`, quindi si allinea il tipo che la contiene. `N` = dimensione
/// effettiva del binario.
#[repr(C, align(4096))]
struct Aligned<const N: usize>([u8; N]);

/// Incapsula un binario raw come static allineata e genera le funzioni
/// `{phys}` (indirizzo fisico) e `{frames}` (numero di frame).
macro_rules! user_binary {
    ($phys:ident, $frames:ident, $path:literal) => {
        const BIN: &[u8; { include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), $path)).len() }] =
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), $path));

        // La macro e' espansa dentro un sottomodulo: `Aligned` vive nel genitore.
        static BIN_BYTES: super::Aligned<{ BIN.len() }> = super::Aligned(*BIN);

        pub fn $phys() -> u64 {
            (&raw const BIN_BYTES) as u64
        }

        pub fn $frames() -> usize {
            (core::mem::size_of::<super::Aligned<{ BIN.len() }>>() + 4095) / 4096
        }
    };
}

// Ogni macro viene espansa DENTRO il suo modulo: le costanti `BIN`/`BIN_BYTES`
// restano private al modulo e non collidono tra i binari.
//
// Fase 21 (servizi da disco): il kernel embedda SOLO lo storage-TCB
// (init/disk/fs, caricati prima che il FS esista); tutto il resto vive in
// `/bin` e `/test` su /fat e parte via `spawn_image` (syscall 38).
mod init_bin { user_binary!(userinit_phys, userinit_frames, "/../userland/build/userinit.bin"); }
mod fs_bin { user_binary!(userfs_phys, userfs_frames, "/../userland/build/userfs.bin"); }
mod disk_bin { user_binary!(userdisk_phys, userdisk_frames, "/../userland/build/userdisk.bin"); }

use init_bin::{userinit_frames, userinit_phys};
use fs_bin::{userfs_frames, userfs_phys};
use disk_bin::{userdisk_frames, userdisk_phys};

/// RIP iniziale del codice user: mappato a `USER_CODE`.
pub fn entry() -> u64 {
    crate::vmm_user::USER_CODE
}

/// Spawna un processo user (gira in ring 3) con il binario raw `code_phys`
/// (per `code_frames` frame), il nome `name` e il processo `parent` (cui il
/// nuovo processo fara' riferimento come antenato; `None` se creato dal kernel).
/// `io_ranges` = porte I/O (inclusive) consentite a ring 3 per il processo
/// (TSS per-processo, ADR-0006); vuoto = nessuna porta.
/// Ritorna il pid assegnato, o `None` se la copia del binario o la creazione
/// del processo falliscono (es. pool PID/TSS saturo).
fn spawn_user(
    name: &'static str,
    priority: crate::sched::Priority,
    code_phys: u64,
    code_frames: usize,
    parent: Option<usize>,
    parent_chan: Option<usize>,
    io_ranges: &[(u16, u16)],
) -> Option<usize> {
    // Il binario embedded viene COPIATO in frame privati per ogni processo:
    // mappare gli stessi frame fisici a piu' processi condividerebbe .bss/.data
    // mutabili (es. la free-list dell'allocatore di libr), corrompendo lo stato
    // quando due istanze dello stesso binario girano in parallelo (test suite
    // multi-client). Il copy al momento dello spawn e' il prezzo della mancanza
    // di COW: i binari sono piccoli (pochi frame).
    let copy = copy_binary(code_phys, code_frames)?;
    let id = unsafe {
        crate::sched::create_user(name, priority, copy, code_frames, entry(), parent, parent_chan, io_ranges, false)
    }?;
    crate::serial_println!(
        "[user] binary '{}': phys={:#x} copy={:#x} entry={:#x} ({} frame)",
        name,
        code_phys,
        copy,
        entry(),
        code_frames,
    );
    Some(id)
}

/// Alloca `frames` frame contigui e vi copia il contenuto del binario embedded
/// a `code_phys`. Ritorna il phys della copia (privata per il processo).
/// `code_phys` e' un VIRT kernel (indirizzo della `static` embedded, nonostante
/// il nome); `dst` e' PHYS e va scritto via direct map.
fn copy_binary(code_phys: u64, frames: usize) -> Option<u64> {
    let dst = crate::phys_mem::alloc_contiguous(frames)?;
    let src = code_phys as *const u8;
    let out = crate::addr::phys_to_virt(dst) as *mut u8;
    for i in 0..(frames * crate::phys_mem::FRAME_SIZE as usize) {
        unsafe {
            *out.add(i) = *src.add(i);
        }
    }
    Some(dst)
}

/// Spawna un processo user dal binario in memoria del CHIAMANTE (Fase 21,
/// servizi da disco: `spawn_image`). `src/len` e' il raw flat PIC (stesso
/// formato dei `.bin`: entry a `USER_CODE`); la copia va in frame privati
/// con coda azzerata (igiene .bss), come `copy_binary`. Corre col CR3 del
/// chiamante: la sorgente user e' leggibile direttamente. Il nome display
/// arriva dal chiamante (`owned`, validato): transitorio "image" visibile al
/// massimo per un tick prima di `set_owned_name` (solo display, mai ABI).
/// `detached` (Fase 22): il figlio non partecipa alla cascata di morte del
/// parent (ri-parentato a init); deciso dallo spawner via SpawnMeta.
pub fn spawn_image(
    owned: &[u8],
    priority: crate::sched::Priority,
    src: *const u8,
    len: usize,
    parent: Option<usize>,
    parent_chan: Option<usize>,
    io_ranges: &[(u16, u16)],
    detached: bool,
) -> Option<usize> {
    let frame = crate::phys_mem::FRAME_SIZE as usize;
    let frames = len.div_ceil(frame);
    let dst = crate::phys_mem::alloc_contiguous(frames)?;
    // `src` e' user-VA del chiamante (CR3 attivo: lettura diretta); `dst` e'
    // PHYS e va scritto via direct map.
    let out = crate::addr::phys_to_virt(dst) as *mut u8;
    for i in 0..len {
        unsafe {
            *(out).add(i) = *src.add(i);
        }
    }
    for i in len..frames * frame {
        unsafe {
            *(out).add(i) = 0;
        }
    }
    let id = unsafe {
        crate::sched::create_user("image", priority, dst, frames, entry(), parent, parent_chan, io_ranges, detached)
    }?;
    crate::sched::set_owned_name(id, owned);
    Some(id)
}

/// Spawna init, il primo processo user (PID 1). Chiamato dal kernel a boot,
/// prima di qualunque altro processo user, cosi' init sia l'antenato dei servizi
/// che poi creera' via `spawn` (Fase 8.1). Parent e canale `None` (kernel).
pub fn spawn_init() -> usize {
    spawn_user("userinit", crate::sched::Priority::Normal, userinit_phys(), userinit_frames(), None, None, &[])
        .expect("spawn di init fallito")
}

/// Descrizione di un binario embedded, per cercarlo per nome (syscall `spawn`).
struct NamedBinary {
    name: &'static str,
    phys: fn() -> u64,
    frames: fn() -> usize,
    /// Porte I/O (inclusive) consentite a ring 3 per questo processo (TSS
    /// per-processo). `&[]` = nessuna porta. Es. `userdisk` → ATA PIO.
    io_ranges: &'static [(u16, u16)],
    /// Priorita' di scheduling del processo.
    priority: crate::sched::Priority,
}

/// Porte dei controller ATA PIO primario + secondario per il disk driver
/// (Fase 16, `userdisk`: enumerazione master/slave su entrambi i canali).
/// `userfs` non tocca piu' porte (Fase 16.2): qualunque `in/out` li' e' #GP.
const ATA_PIO_RANGES: &[(u16, u16)] = &[
    (0x1F0, 0x1F7),
    (0x3F6, 0x3F7),
    (0x170, 0x177),
    (0x376, 0x377),
];

/// Porte CRTC del cursore hardware VGA per il console server (terminale).
const VGA_CURSOR_RANGES: &[(u16, u16)] = &[(0x3D4, 0x3D5)];

/// Porte PS/2 (dati + stato/comandi) per il driver tastiera `userkbd` (Fase 15).
const KBD_PS2_RANGES: &[(u16, u16)] = &[(0x60, 0x64)];

use crate::sched::Priority;

/// I binari embedded spawabili per nome dalla syscall `spawn`. Fase 21: SOLO
/// lo storage-TCB (init/disk/fs) — il resto parte da disco via `spawn_image`.
/// I processi di servizio (fs) sono `Normal`.
const NAMED_BINARIES: &[NamedBinary] = &[
    NamedBinary { name: "userfs",      phys: userfs_phys,      frames: userfs_frames,      io_ranges: &[], priority: Priority::Normal },
    NamedBinary { name: "userdisk",    phys: userdisk_phys,    frames: userdisk_frames,    io_ranges: ATA_PIO_RANGES, priority: Priority::Normal },
    NamedBinary { name: "userinit",    phys: userinit_phys,    frames: userinit_frames,    io_ranges: &[], priority: Priority::Normal },
];

/// Crea un nuovo processo dal binario embedded chiamato `name`. `parent` e'
/// il pid del creatore, `parent_chan` e' il canale di nascita (ADR-0008) che
/// il figlio usera' come canale 0 verso il parent (creato dallo scheduler).
/// Ritorna il pid, o `None` se il nome non e' noto o la creazione fallisce.
pub fn spawn_named(name: &str, parent: Option<usize>, parent_chan: Option<usize>) -> Option<usize> {
    let bin = NAMED_BINARIES.iter().find(|b| b.name == name)?;
    // Si usa il nome 'static della tabella (non il buffer temporaneo del
    // chiamante) perche' il Process conserva un `&'static str`.
    spawn_user(bin.name, bin.priority, (bin.phys)(), (bin.frames)(), parent, parent_chan, bin.io_ranges)
}
