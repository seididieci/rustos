# ADR-0021: Loader ELF per-segmento (W^X del binario)

## Status

Accepted (Fase 31).

## Context

Dalla Fase 6 il kernel carica i binari utente come **immagine flat**: ogni
binario e' linkato a `USER_CODE` con un linker script, convertito in `.bin`
con `objcopy -O binary` e mappato come **un unico mapping RWX** (writable +
executable). La Fase 29 ha aggiunto NX a heap, stack, `mmap` e pagine
iniettate, ma il binario stesso e' rimasto RWX: codice e dati condividono una
regione e non conosciamo il confine. Questo lascia aperta la via classica
"scrivi dati, eseguili" (W^X non rispettato per il codice del processo).

L'ELF generato dal linker contiene gia' l'informazione: 4 segmenti `PT_LOAD`
con flag `R E` (.text), `R` (.rodata), `RW` (.dynamic/.got) e `RW`
(.data/.bss). Il binario e' PIC e le `R_X86_64_RELATIVE` sono gia' applicate
dal linker (`--apply-dynamic-relocs`): poiche' carichiamo sempre al vaddr di
link, **nessuna reloc e' necessaria a runtime**.

## Decision

Il kernel embedda/inietta l'ELF **stripped** (`objcopy --strip-all`, program
header conservati) e lo carica **per-segmento** con un loader minimale
(`kernel/src/elf.rs`):

- `validate(bytes) -> Option<Layout>`: nessuna allocazione; controlla
  magic/classe/dati/macchina/`e_type`, `e_phnum`/`e_phoff`, e per ogni
  `PT_LOAD` `p_filesz <= p_memsz`, `p_offset+p_filesz` nel file, `p_align`
  potenza di 2, `p_vaddr` in `[USER_CODE, USER_FS_BUFFER)` (2 MiB), entry
  dentro un segmento eseguibile. Calcola il range page-aligned e **rifiuta
  una pagina W+X** (invariante di sicurezza).
- `load(cr3, bytes, layout)`: alloca un blocco contiguo di pagine, lo azzera,
  copia i file bytes di ogni segmento all'offset `p_vaddr - base` (buchi e
  bss restano zero), e mappa ogni pagina con i flag dell'ELF (`W`→RW,
  `X`→RX, else RO; NX su tutto tranne il codice), owned.

Il codice diventa **RX**, i dati **RW**, la rodata **RO**: il processo non
puo' piu' scrivere ed eseguire la stessa pagina. La validazione avviene
prima di ogni allocazione, cosi' un ELF malformato (input da disco non
fidato via `spawn_image`) non leakka frame.

## Consequences

### Positive

- W^X reale per il binario: chiude il limite dichiarato in Fase 29 (29b).
- Formato standard e self-describing: niente metadata custom; il loader
  sniffa il magic, quindi i nomi `.bin` e tutti i path restano invariati.
- Il binario embedded e' piu' piccolo (il bss non e' piu' materializzato:
  `userdisk` 185 KiB → 52 KiB) e il kernel cala (732 KiB → 584 KiB).
- Base per future estensioni (segmenti `PT_LOAD` multipli, `.rodata` RO,
  caricamento a vaddr diversi).

### Negative

- Il loader e' codice sensibile (parse di ELF da disco): validazione stretta
  obbligatoria, coperta dal test `code-write`.
- Allocazione **contigua** dell'immagine (≤ 2 MiB): come il vecchio
  `copy_binary`, sotto frammentazione pesante puo' fallire (OOM → panic).
- Le pagine condivise tra segmenti (vaddr non allineate) richiedono di
  unire i flag: risolto calcolando i flag per pagina e rifiutando W+X.

### Neutral

- `--strip-all` conserva i program header; l'ELF resta un eseguibile DYN
  valido.
- Nessuna reloc a runtime: dipende dal caricamento al vaddr di link
  (documentato in `elf.rs`).

## Alternatives Considered

- **Flat `.bin` + header con `rw_off`**: un header di 16 B con l'offset del
  confine RX/RW. Piu' semplice (~40 righe), ma formato custom, senza RO
  distinto e senza validazione per-segmento: scartato perche' l'ELF e' gia'
  self-describing e standard.
- **Parsing dell'ELF solo a build-time** (generare una costante `rw_off`):
  non copre i binari da disco (`spawn_image`), che hanno bisogno di
  self-descrizione a runtime.
- **COW / demand paging**: darebbe anche il non-copy dello spawn, ma e'
  fuori scope (page-in dal fault handler verso userfs e' deadlock-prone).

## References

- `kernel/src/elf.rs`, `kernel/src/vmm_user/paging.rs` (`map_user_leaf`),
  `kernel/src/user_binary.rs`, `kernel/src/process.rs`
- `docs/src/04-memory.md` (sezione "Caricamento ELF")
- ADR-0020 (higher-half), Fase 29 (protezioni/NX)
