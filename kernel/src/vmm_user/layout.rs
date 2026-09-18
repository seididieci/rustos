// Split from vmm_user.rs (byte-identical move; see facade).
/// Indirizzo virtuale del codice user (inizio della regione user).
/// Single source in `syscall-numbers` (Fase 31: serve anche al loader/test).
pub use syscall_numbers::USER_CODE;
pub const USER_BASE: u64 = USER_CODE;
pub(super) const USER_PRESENT_WRITABLE: u64 = 0x4 | 0x3; // U + P + W
/// Bit NX sulla PTE (bit 63, Fase 29): richiede EFER.NXE (abilitato in
/// `boot.asm`). Valido solo sulle foglie: messo sui livelli intermedi
/// renderebbe non-eseguibile l'intero sottoalbero (incluso USER_CODE).
pub(super) const PTE_NX: u64 = 1 << 63;
/// Maschera del campo indirizzo fisico di una PTE (bit 12..51): da usare
/// SEMPRE per estrarre il phys, mai `& !0xFFF` (che con NX lascerebbe il bit
/// 63, corrompendo il frame address e facendo panicare `phys_mem::free`).
pub(super) const PTE_ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
/// Flag foglia per pagine dati user (29): RW + NX. Usato da tutti i mapping
/// foglia tranne il codice (unico eseguibile) e i livelli tabella.
pub(super) const USER_LEAF_RW: u64 = 0x4 | 0x3 | PTE_NX; // U + P + W + NX
/// Flag foglia read-only (29): per VMA PROT_READ materializzate lazy.
pub(super) const USER_LEAF_RO: u64 = 0x4 | 0x1 | PTE_NX; // U + P + NX
/// Bit "owned" software sulla PTE (bit 9 AVL, Fase 14/ADR-0010): la pagina
/// e' DI PROPRIETA' di questo processo (codice copiato, stack user, ring,
/// heap demand-zero) e va liberata al teardown. Le pagine iniettate da altri
/// (`map_physical`/`map_in`: VGA, ring di un client, scratch) NON hanno il
/// bit: il loro owner (il processo che le ha allocate) le libera.
pub(super) const USER_OWNED: u64 = 0x200;
pub(super) const PAGE_SIZE: u64 = 0x1000;

/// Indirizzo virtuale della finestra request ring del processo corrente.
pub const USER_FS_BUFFER: u64 = USER_BASE + 0x200_000;

/// Indirizzo virtuale della finestra response ring del processo corrente.
pub const USER_RESP_RING: u64 = USER_BASE + 0x210_000;

/// Top dello stack user (cresce verso il basso, qui sopra il codice).
/// Esteso a 4 MiB per accommodare VGA, FS buffer, e stack.
/// Single source in `syscall-numbers` (29: serve anche ai test per la guard).
pub use syscall_numbers::USER_STACK_TOP;
/// Numero di frame (4 KiB) dello stack user.
pub use syscall_numbers::USER_STACK_FRAMES;
/// Pagina guard sotto lo stack (29): mai mappata, vedi `syscall-numbers`.
pub use syscall_numbers::USER_STACK_GUARD;

/// Base dello heap on-demand dei processi user: parte vuota subito sopra lo
/// stack e cresce verso l'alto via `sbrk` (syscall 25). Le pagine sotto il
/// `heap_brk` corrente vengono materializzate lazy dal page-fault handler
/// (demand-zero): nessun frame riservato a priori.
pub const USER_HEAP_BASE: u64 = USER_STACK_TOP;

/// Tetto "soft" dello heap: 512 GiB di VA dentro il primo entry PML4 user
/// (non e' un cap pratico: la memoria fisica viene assegnata solo quando le
/// pagine vengono toccate). Serve solo a evitare overflow patologici.
pub const USER_HEAP_LIMIT: u64 = USER_BASE + 0x20_0000_0000;
/// Numero massimo di processi tracciati per lo heap.
pub(super) const MAX_PROCS: usize = 128;
/// Base della zona mmap (1M) e tetto (1G: un PD intero, oltre si espande).
pub const MMAP_BASE: u64 = 0x10_0000;
pub const MMAP_END: u64 = 0x4000_0000;
