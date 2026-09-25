# ADR-0038: Provider trait per filesystem (Fase 46)

**Status**: Implemented (Fase 46 — gate 5/5 + 7/7 + 57/57, zero FAIL/PANIC;
Fase 47/U1 wiring handler via trait per ramfs, gate 5/5 + 7/7 + 57/57;
Fase 48/U2 wiring FAT32 via `LocalFsDyn`, fix readonly stat, gate 5/5 + 7/7 + 57/57;
Fase 49/T0 terreno pre-ArcaFS: handle unico `AnyHandle`, mount-id stabili,
sorgente generica + fstype, `Local` esercitato, create/truncate assorbiti,
gate 5/5 + 7/7 + 57/57).

## Context

Il file system server `userfs` gestisce ramfs e FAT32 tramite enum dispatch
esplicito (`match` sui tipi nei vari handler): ogni nuovo filesystem richiede
modifiche a `mount.rs`, `main.rs`, handlers (routing per nome, mount specs,
lazy activation). Per il futuro ArcaFS serve un'astrazione che consenta di
montare filesystem arbitrari senza modificare il core di userfs.

La presentazione POSIX e' gia' definita in `userfs` (`open/read/write/close/
readdir/stat/mkdir/remove`) — non serve introdurre nuove primitive, solo
incapsulare l'enum dispatch in una trait.

## Decision

**Trait `LocalFs` con object-safe dynamic dispatch via `LocalFsDyn`**.

### 46.0 — Trait `LocalFs`

```rust
pub trait LocalFs {
    type Handle: Copy + PartialEq;

    fn open(&mut self, rel: &str, flags: u32) -> Result<Self::Handle, u64>;
    fn read(&mut self, h: Self::Handle, off: usize, buf: &mut [u8])
        -> Result<usize, u64>;
    fn write(&mut self, h: Self::Handle, off: usize, buf: &[u8], append: bool)
        -> Result<usize, u64>;
    fn close(&mut self, h: Self::Handle);
    fn readdir(&mut self, rel: &str, out: &mut dyn EntrySink)
        -> Result<usize, u64>;
    fn stat(&mut self, rel: &str) -> Result<Meta, u64>;
    fn mkdir(&mut self, rel: &str) -> Result<(), u64>;
    fn remove(&mut self, rel: &str) -> Result<(), u64>;
}
```

`EntrySink`: trait per output readdir (nome nel buffer del client).
`Meta`: metadati POSIX (`size`, `kind`, `readonly`, `mtime`).

### 46.1 — Dynamic dispatch object-safe

La trait non e' object-safe perche' `Handle` e' un type parameter. Soluzione:
`LocalFsDyn` con handles erasure a `*const ()`:

```rust
pub trait LocalFsDyn {
    fn open_dyn(&mut self, rel: &str, flags: u32) -> Result<*const (), u64>;
    fn read_dyn(&mut self, h: *const (), off: usize, buf: &mut [u8])
        -> Result<usize, u64>;
    // ... (write, close, readdir, stat, mkdir, remove)
}
```

`DynHandle<T>` adatta `T: LocalFs` a `LocalFsDyn`: boxa gli handle su open
(`Box::into_raw`) e li libera su close (`Box::from_raw`).

### 46.2 — Enum `MountedFs`

Esteso con varianti per filesystem locali generici:

```rust
pub enum MountedFs {
    Fat(Option<Fat32<IpcDisk>>),
    Local(Box<dyn LocalFsDyn>),
}
```

`Fat`: mantiene la logica esistente (BPB, cluster chain, IPC disk client).
`Local`: wrapper per `DynHandle<T>` con qualsiasi `T: LocalFs`.

### 46.3 — Implementazioni

- `RamFs implements LocalFs` — wrapper sull'implementazione esistente; le
  operazioni delegano ai metodi originali (`find`, nodi BTree).
- `Fat32<B> implements LocalFs` — wrapper su `IpcDisk` (Fase 16/21);
  le operazioni delegano a `read_file`, `write_file`, `read_dir`, ecc.

### 46.4 — Routing handler (U1, Fase 47)

Implementato: gli handler userfs instradano ramfs via `LocalFs` trait:
- `handle_open`: `LocalFs::open(fs, path, flags)` per ramfs
- `handle_read`: `LocalFs::open` + `LocalFs::read` per ramfs
- `handle_write_local`: `LocalFs::open` + `LocalFs::write` per ramfs
- `handle_readdir`: `LocalFs::readdir` per ramfs
- `handle_stat`: `LocalFs::stat` per ramfs
- `handle_mkdir`: `LocalFs::mkdir` per ramfs (fix: esiste → ERR_EXISTS)
- `handle_delete`: `LocalFs::remove` per ramfs

FAT32 resta sulla variande `Fat` (lazy reactivate, IPC disk client specifici).
Zero behavioral regression; gate 5/5 + 7/7 + 57/57.

### 48.0 — Routing handler (U2, Fase 48)

Implementato: gli handler userfs instradano FAT32 via `LocalFsDyn` trait:
- `handle_read`: `LocalFsDyn::read_dyn` per FAT; l'handle e' il `FileInfo`
  della cache per-fd (Fase 21) passato come puntatore allo stack (niente heap
  per-op, niente reopen per path = niente find per read: era una regressione
  ~8x sui load da disco, misurata in t27/t28/t32 e corretta)
- `handle_write_local`: `LocalFsDyn::write_dyn` per FAT, stessa cache; dopo la
  mutazione la cache e' rinfrescata con un find fresco (size/first_cluster
  possono cambiare); O_APPEND dal flag (contratto ramfs)
- `handle_open`: validazione via `LocalFsDyn::stat_dyn` per FAT; O_CREAT
  (`create_file`) e O_TRUNC (`truncate`) restano FAT-specifici; `open_fat`
  popola la cache FileInfo del fd
- `handle_readdir`: `LocalFsDyn::readdir_dyn` per FAT (sink inline, poi union
  con mount annidati)
- `handle_stat`: `LocalFsDyn::stat_dyn` per FAT; `stat_kind` propaga
  `Meta.readonly` in `STAT_READONLY` (prima ignorato: `libr::stat` lo
  decodifica ma nessun handler lo scriveva)

`Fat32<B>` implementa `LocalFsDyn` con handle `AnyHandle::Fat` by-value
(Fase 49: niente piu' handle boxati). `O_CREAT`/`O_TRUNC` assorbiti in
`Fat32::open` (come `RamFs::open`); restano fuori trait solo cache per-fd
e generazione (stato userfs, per disegno). Zero behavioral regression;
gate 5/5 + 7/7 + 57/57.

### 49.0 — Terreno pre-ArcaFS (Fase 49, un solo gate)

Chiude i debiti 46-48 emersi dalla review pre-ArcaFS (object-store futuro):

- **F0 dettagli**: `RamHandle::new` → `Option` (mai troncamento silenzioso
  oltre 64 B); `Fat32::open` su single-source `libr::O_CREAT`/`O_TRUNC`
  (mai `0x200` magico); niente dummy `FileInfo` cluster 0 (find fallito
  dopo create = errore, mai handle invalido).
- **F1 handle unico**: `AnyHandle { Ram(RamHandle), Fat(FileInfo) }`
  by-value; `open_dyn` ritorna l'handle (niente `Box`, niente raw-pointer),
  `close_dyn` rimossa con `DynHandle` (entrambe le `close` concrete no-op).
  Chiude type-confusion, free-di-stack e double-free latenti; per-op
  heap-free (regola Fase 24). `RamFs` implementa `LocalFsDyn` come `Fat32`
  (match sul ramo sbagliato = `ERR_INVALID`).
- **F2 mount-id stabili**: `FsMount.id: u64` monotonico (`next_mount_id` in
  `server.rs`, mai riusato); gli fd tengono l'id (`by_id`/`by_id_mut`,
  `reactivate_mount_by_id`); `umount` orfana gli fd (errore al prossimo
  uso) invece di aliasare il vicino shiftato dal `remove`.
- **F3 sorgente generica + fstype**: `enum Source::Block { key }` +
  `negotiate()` (superblock, oggi solo vfat → `("vfat", Fat)`); campo
  `fstype` per mount; rimosso `handle: u32` (scritto e mai letto).
  `resolve_mount_source` resta per gli open raw by-path.
- **F4 `Local` esercitato end-to-end**: `R_MOUNT "ramfs"` monta
  un'istanza ramfs tmpfs-like (sempre attiva, no IPC, `reactivate` no-op,
  `note_peer_death` no-op) + path fd completo per i mount `Local`:
  `open_dyn` → `AnyHandle` in ftable (`open_local`, `FsKind::Local`,
  `get_dyn_handle`), read/write via handle dell'fd, `lseek` END via
  `stat_dyn`, mkdir/delete via trait; `resolve_local` distingue Fat da
  Local sul match longest-prefix. La variante `Local` e' viva per davvero.
- **F5 create/truncate assorbiti**: `handle_open` FAT = una `open` via
  trait sul concreto (`fat_mut`); bump `fgen` solo a `O_CREAT`/`O_TRUNC`.
  Restano fuori trait (per disegno): cache `FileInfo` per-fd, `fgen`,
  `lseek` SEEK_END (serve l'fd, non il path).

## Consequences

### Positive

- **Estensibilita'**: un nuovo filesystem si monta come
  `MountedFs::Local(Box::new(DynHandle(ArcaFs::new(...))))` senza toccare
  il core di userfs.
- **Single source of truth**: la trait definisce la presentazione POSIX;
  ramfs e fat32 condividono lo stesso interfaccia per i test futuri.
- **Zero runtime change**: gli handler non usano ancora `Local`; nessun
  cambiamento di comportamento osservabile.

### Negative (chiusi in Fase 49)

- **Heap per-op**: superato — niente piu' `Box` per-open (`AnyHandle`
  by-value, entrambe le `close` concrete no-op). La regola Fase 24
  ("mai heap nel per-op") vale di nuovo su tutti i path.
- **Variante `Local` mai costruita**: superato — `R_MOUNT "ramfs"` la
  esercita (F4); la prova di estensibilita' esiste davvero.

### Neutral

- **Fat32 nel path Local**: U2 (Fase 48) ha instradato FAT32 via `LocalFsDyn`
  in tutti gli handler; la Fase 49 ha assorbito anche create/truncate in
  `open`. Restano fuori trait per disegno (stato userfs, non del FS):
  cache `FileInfo` per-fd + `fgen`, `lseek` SEEK_END. Lazy reactivate
  (`IpcDisk` reconnect) e cache settoriale (Fase 25) restano specifiche
  ma non impediscono l'unificazione del path principale.

## Alternatives Considered

### Generico su trait object vs enum match diretto

Un `enum { RamFs, Fat32 }` con macro di dispatch e' piu' leggero (zero box,
zero dynamic dispatch) ma meno estensibile (ogni nuovo filesystem = nuova
branch del match + modifiche agli handler). La trait astrae il pattern
"open→read/write→close" in un interfaccia standard.

### Inline handles vs boxed handles

Invece di `Box::into_raw`/`Box::from_raw`, si potria usare un allocator a
slot (array fisso con bitmap) — zero alloc heap, handle = indice slot.
Ma introduce complessita' (lifecycle, leak se manca close) e non giustifica
il costo per U0.

### Unified `MountedFs` per FAT32 + Local (storico: superato in Fase 49)

Unificare FAT32 nel path `Local` eliminerebbe la variande `Fat`. Al tempo
di U0 si scelse la separazione (FAT32 con `IpcDisk`, cache settoriale,
lazy reactivate: troppo specifico). U2/Fase 48 ha poi instradato FAT32 via
`LocalFsDyn` comunque, e la Fase 49 ha unificato anche create/truncate:
la separazione resta solo per lo stato userfs (cache per-fd, epoche),
non per il dispatch. Tenere le due varianti e' ormai solo comodo per
`reactivate`/`note_peer_death` (epoche disco), non un limite del trait.

## Riferimenti

- [09-filesystem](../09-filesystem.md): architettura userfs, mount table
- [ADR-0013](./0013-mount-syscall.md): mount espliciti
- [Fase 24](../13-performance.md): "mai heap nel per-op dei server"
