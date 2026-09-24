# ADR-0038: Provider trait per filesystem (Fase 46)

**Status**: Implemented (Fase 46 — gate 5/5 + 7/7 + 57/57, zero FAIL/PANIC).

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

### 46.4 — Routing handler (U1, non implementato)

Gli handler attuali usano ancora l'enum dispatch esplicito (`match` su
`MountedFs`). Il prossimo passo (U1) e' instradare ramfs via `Local` +
`dyn`, lasciando FAT32 sulla variande `Fat` (lazy reactivate, IPC disk
client sono specifici).

## Consequences

### Positive

- **Estensibilita'**: un nuovo filesystem si monta come
  `MountedFs::Local(Box::new(DynHandle(ArcaFs::new(...))))` senza toccare
  il core di userfs.
- **Single source of truth**: la trait definisce la presentazione POSIX;
  ramfs e fat32 condividono lo stesso interfaccia per i test futuri.
- **Zero runtime change**: gli handler non usano ancora `Local`; nessun
  cambiamento di comportamento osservabile.

### Negative

- **Dead code warnings**: finche' gli handler non instradano via `Local`,
  `DynHandle`, `Meta`, `EntrySink` e le impl `RamFs`/`Fat32` sono codice
  morto (~20 warning). Va silenziato o risolto in U1.
- **Heap per-op**: `DynHandle::open_dyn` boxa ogni handle (`Box::new(h)`).
  La regola Fase 24 dice "mai heap nel per-op dei server". Per un filesystem
  locale con handles piccoli (u32, usize) e lifecycle chiuso (open→use→close
  nello stesso IPC), l'overhead e' trascurabile; ma se il path diventa hot
  va sostituito con un allocator a slot (o `LocalFs::Handle` inline).

### Neutral

- **Fat32 non ancora nel path Local**: rimane sulla variande `Fat` perche'
  la lazy reactivate (`IpcDisk` reconnect) e il cache settoriale (Fase 25)
  sono specifici. U1 puo' unificare se il pattern si generalizza.

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

### Unified `MountedFs` per FAT32 + Local

Unificare FAT32 nel path `Local` eliminerebbe la variande `Fat`. Ma FAT32
tiene `IpcDisk` (client IPC verso userdisk), cache settoriale (Fase 25),
lazy reactivate: e' troppo specifico per un trait generico. Meglio tenere
la separazione attuale e instradare solo ramfs via `Local` in U1.

## Riferimenti

- [09-filesystem](../09-filesystem.md): architettura userfs, mount table
- [ADR-0013](./0013-mount-syscall.md): mount espliciti
- [Fase 24](../13-performance.md): "mai heap nel per-op dei server"
