//! Convenzione argv (Fase 37.1): argc/argv sullo stack iniziale, layout stile
//! Linux come CONVENZIONE DI DATI neutra (ADR-0025 §Neutral: formato
//! versionabile, mai struttura kernel — serve alla personalita' POSIX, il
//! nativo resta libero di ignorarlo).
//!
//! Il kernel scrive all'entry (spawn: argc=0; exec: gli argv passati):
//! `[rsp]=argc, [rsp+8]=argv[0], ..., NULL, envp NULL, stringhe NUL-terminate`
//! in ordine Linux (stringhe in alto, argc in basso: tutto sopra rsp, mai
//! toccato dalla red zone che cresce verso il basso).
//!
//! `entry!(main)` genera lo `_start` naked — unico punto che tocca rsp
//! all'ingresso, prima di qualunque prologo Rust — e salta a `main(sp)` con
//! rsp invariato. `main` parsa con `args_from_stack(sp)`. Sostituisce gli
//! `_start` scritti a mano (stesso simbolo/sezione/ABI): il nostro CRT
//! minimale, esplicito — non uno strato runtime.

use super::*;

/// Bound del blocco argv serializzato (decisione 37.1, single source col
/// kernel in `syscall-numbers::ARGS_MAX` qui riesportata per i client):
/// oltre il kernel rifiuta fail-loud.
pub use syscall_numbers::ARGS_MAX;

/// Argomenti di avvio: vista borrowed sullo stack iniziale (vive quanto il
/// processo: lo stack iniziale non viene mai smappato).
#[derive(Clone, Copy, Debug)]
pub struct Args {
    argc: u64,
    argv: u64,
}

impl Args {
    /// Numero di argomenti (0 per spawn senza exec).
    pub fn argc(&self) -> u64 {
        self.argc
    }
    /// i-esimo argomento come byte (senza NUL), o `None` se fuori range o
    /// malformato. Layout atteso (ordine Linux, indirizzi crescenti):
    /// `[argc][argv[0..n]][NULL][envp NULL][stringhe...]`, tutto entro
    /// `ARGS_MAX` dallo stack pointer iniziale.
    pub fn get(&self, i: u64) -> Option<&[u8]> {
        if i >= self.argc {
            return None;
        }
        // Fine dell'array (argv[] + NULL + envp NULL): le stringhe stanno sopra.
        let arr_end = self
            .argv
            .checked_add(8 * self.argc.checked_add(2)?)?;
        let ptr = unsafe { core::ptr::read((self.argv + i * 8) as *const u64) };
        let cap = (self.argv.wrapping_sub(8)).checked_add(ARGS_MAX)?;
        if ptr < arr_end || ptr >= cap {
            return None;
        }
        let mut len = 0u64;
        while ptr + len < cap {
            let b = unsafe { core::ptr::read((ptr + len) as *const u8) };
            if b == 0 {
                return Some(unsafe {
                    core::slice::from_raw_parts(ptr as *const u8, len as usize)
                });
            }
            len += 1;
        }
        None
    }
}

/// Parsifica gli argomenti dallo stack pointer iniziale `sp` (catturato dallo
/// shim `entry!` prima di qualunque prologo). `None` = layout invalido
/// (argc assurdo: stack corrotto o bug kernel — il chiamante esce loud).
/// La lettura oltre il bound non e' tentata (vedi `Args::get`); un fault su
/// puntatore spazzatura dentro la finestra resta fail-loud via fault→kill.
/// Ultimo argv con magic redirect (40.4c) = nascosto: `argc` e' gia' al netto
/// (il programma non vede mai la spec; il claim vive in `stdio_restore`).
pub fn args_from_stack(sp: u64) -> Option<Args> {
    let argc = unsafe { core::ptr::read(sp as *const u64) };
    if argc > 1024 {
        return None;
    }
    if argc > 0 && has_redir_magic(sp, argc) {
        return Some(Args { argc: argc - 1, argv: sp + 8 });
    }
    Some(Args { argc, argv: sp + 8 })
}

/// Legge l'ultimo argv grezzo dallo stack (stessi bound di `Args::get`).
/// `None` = argc 0/assurdo o ultimo arg fuori finestra/malformato.
/// Lifetime 'static: lo stack iniziale non viene mai smappato (vive quanto il
/// processo, come la vista `Args`).
fn last_arg_raw(sp: u64, argc: u64) -> Option<&'static [u8]> {
    if argc == 0 || argc > 1024 {
        return None;
    }
    let argv = sp + 8;
    let arr_end = argv.checked_add(8 * argc.checked_add(2)?)?;
    let ptr = unsafe { core::ptr::read((argv + (argc - 1) * 8) as *const u64) };
    let cap = (argv.wrapping_sub(8)).checked_add(ARGS_MAX)?;
    if ptr < arr_end || ptr >= cap {
        return None;
    }
    let mut len = 0u64;
    while ptr + len < cap {
        let b = unsafe { core::ptr::read((ptr + len) as *const u8) };
        if b == 0 {
            return Some(unsafe {
                core::slice::from_raw_parts(ptr as *const u8, len as usize)
            });
        }
        len += 1;
    }
    None
}

/// True se l'ultimo argv ha il prefisso magic redirect (spec valida o no: un
/// arg craftato col magic si nasconde comunque — dalla shell e' impossibile
/// produrne uno, `read_line` filtra 0x7f).
pub(crate) fn has_redir_magic(sp: u64, argc: u64) -> bool {
    match last_arg_raw(sp, argc) {
        Some(a) => a.starts_with(crate::stdio::REDIR_MAGIC),
        None => false,
    }
}

/// Ritorna l'ultimo argv se ha il magic redirect (validita' voci a carico di
/// `stdio_restore`). `None` = nessun redirect per questo processo.
pub(crate) fn redir_spec_arg(sp: u64) -> Option<&'static [u8]> {
    let argc = unsafe { core::ptr::read(sp as *const u64) };
    if argc == 0 || argc > 1024 || !has_redir_magic(sp, argc) {
        return None;
    }
    last_arg_raw(sp, argc)
}

/// Genera l'entry point `_start` (CRT minimale): naked shim che passa lo stack
/// pointer iniziale a `__velordor_entry(sp)` con un salto (`jmp`, rsp invariato —
/// una `call` sporcherebbe lo stack con l'indirizzo di ritorno).
/// `__velordor_entry` ripristina i redirect da argv-magic (`stdio_restore`,
/// Fase 40.4c: nessuno effetto senza magic) poi salta a `$main(sp)` con rsp
/// ripristinato (stesso stato di un ingresso diretto: `rsp` iniziale in rdi).
/// `$main` e' una normale funzione Rust `fn(u64) -> !` (primo argomento in rdi,
/// ABI SysV). Stesso simbolo/sezione degli `_start` scritti a mano che
/// sostituisce.
#[macro_export]
macro_rules! entry {
    ($main:ident) => {
        #[unsafe(no_mangle)]
        #[unsafe(naked)]
        pub extern "C" fn _start() -> ! {
            ::core::arch::naked_asm!(
                "mov rdi, rsp",
                "jmp {entry}",
                entry = sym __velordor_entry,
            );
        }
        fn __velordor_entry(sp: u64) -> ! {
            $crate::stdio_restore(sp);
            unsafe {
                ::core::arch::asm!(
                    "mov rsp, {sp}",
                    "mov rdi, {sp}",
                    "jmp {main}",
                    sp = in(reg) sp,
                    main = sym $main,
                    options(noreturn),
                )
            }
        }
        // `$main` e' referenziata solo dall'asm sopra: senza questo root
        // `--gc-sections` la scarterebbe (undefined symbol al link).
        #[used]
        static _VELORDOR_ENTRY_KEEP: fn(u64) -> ! = $main;
    };
}
