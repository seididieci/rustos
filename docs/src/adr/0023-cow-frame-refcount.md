# ADR-0023: COW a livello di frame (frame refcount + COW fault)

## Status

Accepted (implementata, Fase 33 — gate 5/5 + 7/7 + 48/48 + shell 30/30,
zero FAIL/PANIC/FAULT).

## Context

La Fase 30 ha introdotto la memoria condivisa (`shm`) e la Fase 32 la shared
text ELF: le pagine immutabili del binario sono condivise tra istanze dello
stesso processo, con un **refcount per-oggetto** (regione `shm`, text image).
Queste primitive condividono pagine *read-only* o *writable-shared*, ma non
offrono il **copy-on-write**: una pagina condivisa e' sempre condivisa, oppure
privata dall'inizio.

Serve invece il COW per il **`fork`** (Fase 34): il figlio deve condividere
*arbitrarie* pagine (gia' scritte) del padre, e separarsi solo al primo write.
Un refcount per-oggetto non basta: le pagine di un address space non
appartengono a un singolo oggetto condiviso, e una pagina puo' passare da
condivisa a privata in modo indipendente dalle altre. Serve quindi un refcount
**per-frame** e un fault handler che materializzi la copia privata.

Nota: il COW "sull'immagine di exec" (condividere anche il `.data` del binario)
e' stato valutato e scartato: per un processo singolo la pagina scritta
risulterebbe duplicata (copia nell'immagine + copia privata), con un guadagno
trascurabile (la parte scrivibile dei binari e' ~5 KiB). Il percorso `exec`
resta lo split della Fase 32; il COW generale serve a `fork`.

## Decision

**Frame refcount** (`phys_mem.rs`): un array di 1 byte per frame, allocato a
boot subito dopo la bitmap (dinamico, stesso schema di `BITMAP_PTR`: evita un
`.bss` di 16 MiB a 64 GiB). `alloc`/`alloc_contiguous` impostano `ref = 1`;
nuovi `deref(frame)` (ref--, a 0 libera) e `deref_contiguous`. `free`/
`free_contiguous` restano per i frame a ref 1 (page table, stack kernel, ring,
text image, shm non-COW).

**COW fault**: bit software `USER_COW = 0x400` (bit 10 AVL; `OWNED` e' bit 9).
`cow_fault(cr3, addr) -> bool`: se la PTE e' `present && COW && !W`, alloca un
frame (ref 1), copia 4 KiB dal vecchio (via direct map), rimappa
`owned|RW|NX` (azzera COW), `deref` il vecchio, `invlpg`. Nel page-fault
handler, ramo protection-violation, si tenta **prima** `cow_fault` (vale sia
per fault user sia supervisor: il kernel puo' scrivere buffer user), poi
kill/halt. Le protection-violation su codice/rodata (senza COW) continuano a
uccidere il processo.

**Free delle foglie user → `deref`**: `teardown.rs::free_pt_leaves` e
`vma.rs::unmap_user_range` (foglie `owned`) usano `deref` invece di `free`:
un frame condiviso (ref>1) sopravvive al teardown del primo sharer.

**Primitiva testabile**: `shm_map` con flag `MAP_COW` mappa i frame della
regione `RO`+`COW` e incrementa il ref per mappatura (la regione tiene il ref
di allocazione); sul COW fault il frame della regione e' `deref`-ato; `munmap`/
teardown `deref` per i frame ancora condivisi; `shm_release` a 0
`deref_contiguous`. Semantica: due processi mappano la stessa regione COW →
leggono gli stessi dati finche' non scrivono, poi isolati.

## Consequences

### Positive

- Meccanismo **generale** di condivisione con COW: prerequisito reale di
  `fork`, riusabile per future mappature COW (file-backed, snapshot).
- Il percorso `exec` non cambia (resta lo split della Fase 32): nessuna
  duplicazione della parte scrivibile.

### Negative

- L'array refcount e la conversione dei path di free toccano l'allocatore
  (percorso critico) e il COW fault e' caldo: rischio di bug subdoli
  (use-after-free, doppio free, refcount sbilanciato).
- Un byte/frame di RAM aggiuntiva (piccola: 64 KiB a 256 MiB).

### Neutral

- `free` resta invariato per i frame a ref 1; `deref` solo dove serve (foglie
  user + COW).
- Il contatore `cow` (fault gestiti) e' esposto per il test.

## Alternatives Considered

- **COW sull'immagine di exec** (Fase 32 estesa al `.data`): per un processo
  singolo la pagina scritta e' duplicata (immagine + privata) e il guadagno e'
  trascurabile (~5 KiB); non generalizza a `fork` (l'immagine e' un oggetto
  speciale). Scartata.
- **Refcount per-oggetto soltanto** (`shm`/text image): sufficiente per
  condividere regioni intere, ma non per il `fork`, dove pagine arbitrarie
  diventano private in modo indipendente. Scartata.
- **COW via `shm` soltanto** (senza refcount per-frame): non copre le pagine
  del padre in `fork` (non appartengono a una regione `shm`). Scartata.

## References

- `kernel/src/phys_mem.rs`, `kernel/src/vmm_user/paging.rs` (`cow_fault`),
  `kernel/src/vmm_user/shm.rs` (`MAP_COW`), `kernel/src/interrupts.rs`
- ADR-0022 (shared text), Fase 30 (memoria condivisa)
- Fase 34 (`fork`, ADR-0024) — consumatore del meccanismo
