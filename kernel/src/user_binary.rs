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
mod demo_bin { user_binary!(userdemo_phys, userdemo_frames, "/../testland/build/userdemo.bin"); }
mod init_bin { user_binary!(userinit_phys, userinit_frames, "/../userland/build/userinit.bin"); }
mod console_bin { user_binary!(userconsole_phys, userconsole_frames, "/../userland/build/userconsole.bin"); }
mod uptime_bin { user_binary!(useruptime_phys, useruptime_frames, "/../userland/build/useruptime.bin"); }
mod fs_bin { user_binary!(userfs_phys, userfs_frames, "/../userland/build/userfs.bin"); }
mod testfs_bin { user_binary!(usertestfs_phys, usertestfs_frames, "/../testland/build/usertestfs.bin"); }
mod testfat_bin { user_binary!(usertestfat_phys, usertestfat_frames, "/../testland/build/usertestfat.bin"); }
mod devfs_bin { user_binary!(userdevfs_phys, userdevfs_frames, "/../userland/build/userdevfs.bin"); }
mod kbd_bin { user_binary!(userkbd_phys, userkbd_frames, "/../userland/build/userkbd.bin"); }
mod tty_bin { user_binary!(usertty_phys, usertty_frames, "/../userland/build/usertty.bin"); }
mod shell_bin { user_binary!(usershell_phys, usershell_frames, "/../userland/build/usershell.bin"); }
mod hogheap_bin { user_binary!(userhogheap_phys, userhogheap_frames, "/../testland/build/userhogheap.bin"); }
mod devreader_bin { user_binary!(userdevreader_phys, userdevreader_frames, "/../testland/build/userdevreader.bin"); }
mod usertests_bin { user_binary!(usertests_phys, usertests_frames, "/../testland/build/usertests.bin"); }
mod usertestcli_bin { user_binary!(usertestcli_phys, usertestcli_frames, "/../testland/build/usertestcli.bin"); }
mod usertestspin_bin { user_binary!(usertestspin_phys, usertestspin_frames, "/../testland/build/usertestspin.bin"); }
mod utcbstest_bin { user_binary!(utcbstest_phys, utcbstest_frames, "/../testland/build/utcbstest.bin"); }

use demo_bin::{userdemo_frames, userdemo_phys};
use init_bin::{userinit_frames, userinit_phys};
use console_bin::{userconsole_frames, userconsole_phys};
use uptime_bin::{useruptime_frames, useruptime_phys};
use fs_bin::{userfs_frames, userfs_phys};
use testfs_bin::{usertestfs_frames, usertestfs_phys};
use testfat_bin::{usertestfat_frames, usertestfat_phys};
use devfs_bin::{userdevfs_frames, userdevfs_phys};
use kbd_bin::{userkbd_frames, userkbd_phys};
use tty_bin::{usertty_frames, usertty_phys};
use shell_bin::{usershell_frames, usershell_phys};
use hogheap_bin::{userhogheap_frames, userhogheap_phys};
use devreader_bin::{userdevreader_frames, userdevreader_phys};
use usertests_bin::{usertests_frames, usertests_phys};
use usertestcli_bin::{usertestcli_frames, usertestcli_phys};
use usertestspin_bin::{usertestspin_frames, usertestspin_phys};
use utcbstest_bin::{utcbstest_frames, utcbstest_phys};

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
    io_ranges: &'static [(u16, u16)],
) -> Option<usize> {
    // Il binario embedded viene COPIATO in frame privati per ogni processo:
    // mappare gli stessi frame fisici a piu' processi condividerebbe .bss/.data
    // mutabili (es. la free-list dell'allocatore di libr), corrompendo lo stato
    // quando due istanze dello stesso binario girano in parallelo (test suite
    // multi-client). Il copy al momento dello spawn e' il prezzo della mancanza
    // di COW: i binari sono piccoli (pochi frame).
    let copy = copy_binary(code_phys, code_frames)?;
    let id = unsafe {
        crate::sched::create_user(name, priority, copy, code_frames, entry(), parent, parent_chan, io_ranges)
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
fn copy_binary(code_phys: u64, frames: usize) -> Option<u64> {
    let dst = crate::phys_mem::alloc_contiguous(frames)?;
    let src = code_phys as *const u8;
    let out = dst as *mut u8;
    for i in 0..(frames * crate::phys_mem::FRAME_SIZE as usize) {
        unsafe {
            *out.add(i) = *src.add(i);
        }
    }
    Some(dst)
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
    /// per-processo). `&[]` = nessuna porta. Es. `userfs` → ATA PIO.
    io_ranges: &'static [(u16, u16)],
    /// Priorita' di scheduling del processo.
    priority: crate::sched::Priority,
}

/// Porte del controller ATA PIO primario (drive master) per il fs server.
const ATA_PIO_RANGES: &[(u16, u16)] = &[(0x1F0, 0x1F7), (0x3F6, 0x3F7)];

/// Porte CRTC del cursore hardware VGA per il console server (terminale).
const VGA_CURSOR_RANGES: &[(u16, u16)] = &[(0x3D4, 0x3D5)];

/// Porte PS/2 (dati + stato/comandi) per il driver tastiera `userkbd` (Fase 15).
const KBD_PS2_RANGES: &[(u16, u16)] = &[(0x60, 0x64)];

use crate::sched::Priority;

/// I binari embedded spawabili per nome dalla syscall `spawn`. L'ordine non e'
/// rilevante: la ricerca e' lineare (pochi elementi). I processi "di servizio"
/// (console/fs/devfs/shell) sono `Normal`; quelli puramente informativi o demo
/// (`useruptime`) sono `Low`, cosi' girano solo quando non c'e' lavoro Normal.
const NAMED_BINARIES: &[NamedBinary] = &[
    NamedBinary { name: "userconsole", phys: userconsole_phys, frames: userconsole_frames, io_ranges: VGA_CURSOR_RANGES, priority: Priority::Normal },
    NamedBinary { name: "userdemo",    phys: userdemo_phys,    frames: userdemo_frames,    io_ranges: &[], priority: Priority::Low },
    NamedBinary { name: "useruptime",  phys: useruptime_phys,  frames: useruptime_frames,  io_ranges: &[], priority: Priority::Low },
    NamedBinary { name: "userfs",      phys: userfs_phys,      frames: userfs_frames,      io_ranges: ATA_PIO_RANGES, priority: Priority::Normal },
    NamedBinary { name: "usertestfs",  phys: usertestfs_phys,  frames: usertestfs_frames,  io_ranges: &[], priority: Priority::Normal },
    NamedBinary { name: "usertestfat", phys: usertestfat_phys, frames: usertestfat_frames, io_ranges: &[], priority: Priority::Normal },
    NamedBinary { name: "userdevfs",   phys: userdevfs_phys,   frames: userdevfs_frames,   io_ranges: &[], priority: Priority::Normal },
    NamedBinary { name: "userkbd",     phys: userkbd_phys,     frames: userkbd_frames,     io_ranges: KBD_PS2_RANGES, priority: Priority::Normal },
    NamedBinary { name: "usertty",     phys: usertty_phys,     frames: usertty_frames,     io_ranges: &[], priority: Priority::Normal },
    NamedBinary { name: "usershell",   phys: usershell_phys,   frames: usershell_frames,   io_ranges: &[], priority: Priority::Normal },
    NamedBinary { name: "userhogheap", phys: userhogheap_phys, frames: userhogheap_frames, io_ranges: &[], priority: Priority::Normal },
    NamedBinary { name: "userdevreader", phys: userdevreader_phys, frames: userdevreader_frames, io_ranges: &[], priority: Priority::Normal },
    NamedBinary { name: "usertestcli",   phys: usertestcli_phys,   frames: usertestcli_frames,   io_ranges: &[], priority: Priority::Normal },
    NamedBinary { name: "usertests",     phys: usertests_phys,     frames: usertests_frames,     io_ranges: &[], priority: Priority::Normal },
    // Stesso binario spin esposto a piu' priorita': la suite spawna per nome.
    NamedBinary { name: "usertestspin",  phys: usertestspin_phys,  frames: usertestspin_frames,  io_ranges: &[], priority: Priority::Low },
    NamedBinary { name: "utspin_norm",   phys: usertestspin_phys,  frames: usertestspin_frames,  io_ranges: &[], priority: Priority::Normal },
    NamedBinary { name: "utspin_high",   phys: usertestspin_phys,  frames: usertestspin_frames,  io_ranges: &[], priority: Priority::High },
    NamedBinary { name: "utcbstest",     phys: utcbstest_phys,     frames: utcbstest_frames,     io_ranges: &[], priority: Priority::Normal },
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
