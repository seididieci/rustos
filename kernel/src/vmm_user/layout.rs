// Split from vmm_user.rs (byte-identical move; see facade).
pub const USER_BASE: u64 = 0x0000_4000_0000_0000;
pub(super) const USER_PRESENT_WRITABLE: u64 = 0x4 | 0x3; // U + P + W
/// Bit "owned" software sulla PTE (bit 9 AVL, Fase 14/ADR-0010): la pagina
/// e' DI PROPRIETA' di questo processo (codice copiato, stack user, ring,
/// heap demand-zero) e va liberata al teardown. Le pagine iniettate da altri
/// (`map_physical`/`map_in`: VGA, ring di un client, scratch) NON hanno il
/// bit: il loro owner (il processo che le ha allocate) le libera.
pub(super) const USER_OWNED: u64 = 0x200;
pub(super) const PAGE_SIZE: u64 = 0x1000;

/// Indirizzo virtuale del codice user (inizio della regione user).
pub const USER_CODE: u64 = USER_BASE;

/// Indirizzo virtuale della finestra request ring del processo corrente.
pub const USER_FS_BUFFER: u64 = USER_BASE + 0x200_000;

/// Indirizzo virtuale della finestra response ring del processo corrente.
pub const USER_RESP_RING: u64 = USER_BASE + 0x210_000;

/// Top dello stack user (cresce verso il basso, qui sopra il codice).
/// Esteso a 4 MiB per accommodare VGA, FS buffer, e stack.
pub const USER_STACK_TOP: u64 = USER_BASE + 0x400_000;
/// Numero di frame (4 KiB) dello stack user.
pub const USER_STACK_FRAMES: usize = 4;

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
