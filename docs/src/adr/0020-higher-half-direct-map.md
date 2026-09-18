# ADR-0020: Higher-half kernel + direct map

**Status**: Accettato (27.1/27.2/27.3 implementati, gate verde)
**Data**: 2026-09-17

## Contesto

Dalla Fase 6 il kernel viveva in identity map bassa (1M+), condividendo il
primo quarto dello spazio con... nessuno, ma occupandolo concettualmente: la
nota in AGENTS e `03-memory.md` rimandava l'higher-half "a quando i processi
user richiederanno spazio basso pulito". Quella condizione e' matura: il
censimento 27.1 ha mostrato che il kernel e' l'unico occupante del basso, e il
basso libero sblocca pagine NULL non mappate (oggi `[0,8M)` identity: un
NULL-deref legge spazzatura invece di faultare), assunzioni std/loader e una
netta separazione dei domini (user a `0x4000_…`, kernel in alto).

## Decisione

Kernel alto a `-2G+1M` (`0xFFFF_FFFF_8010_0000`, LMA 1M — il +1M segue Linux
`0xFFFFFFFF81000000`) + direct map di tutta la RAM a
`0xFFFF_8880_0000_0000` a pagine 2M. Zero cambi a: syscall ABI, IPC,
scheduler, indirizzi user, libr, testland, stack disco/FS.

Fasi (ognuna con gate verde + review):

- **27.1** — conversione meccanica a offset zero: nuovo `kernel/src/addr.rs`
  (`phys_to_virt`/`virt_to_phys`/`kern_*`, offset 0) + tutti i siti del
  censimento instradati (choke point `entry_at`/`set_entry`/`zero_frame`,
  `BITMAP_PTR`, stack/RSP0 come VIRT, `boot_info`, `copy_binary`,
  demand-zero, VGA). Nessun cambio di comportamento, prova via gate.
- **27.2** — il flip: `linker.ld` VMA alte + LMA basse (`AT()`), `boot.asm`
  tutto-alto dual-map, `boot_tables.rs` (PML4 dual + PDPT_K/PD_K + direct map
  statica), `vmm.rs` ridotto al top-up/guard, guard seriali a stadi in
  `rust_main`. Gate-0 `readelf` (VMA−LMA == OFFSET su ogni PT_LOAD + nota
  PVH) prima di ogni boot.
- **27.3** — pulizia e chiusura: stack alto dallo stub, `unmap_low()`
  (`PML4[0] = 0` + flush) a inizio `rust_main`, split VGA UC statico,
  selftest NULL-#PF (prova regina del basso libero).

## Dettagli che hanno morso (per chi tocca il boot)

1. **Basi 2M pari o morte (RSVD).** VMA tonda a −2G con LMA a 1M darebbe
   entry PD con basi dispari (bit 20 = riservato per pagine 2M → `#PF` con
   RSVD, errore `0x8`, zero output). Il +1M rende lo scarto VMA-phys congruo
   e tutte le basi pari. Invariante mod 1G verificata dal compilatore
   (`const assert` in `addr.rs`).
2. **Sotto 1M solo 0x90000–0x9FC00 e' RAM.** Il resto e' buco PCI/VGA
   (letture `0xFF`): le 32 PD direct + PT_VGA a LMA 0xB7000 leggevano
   spazzatura (osservato via gdb `xp`: `0xFF...`, fault RSVD in scrittura).
   Le 7 tabelle base restano a 0x90000; PD/PT della direct map a LMA fissa
   16M (`.tables_high`, oltre heap+bitmap in ogni config, verificata RAM da
   `phys_mem::init` fail-loud).
3. **Pagine 2M, non 1G.** Le 1G avrebbero richiesto PDPE1GB, assente sul TCG
   qemu64 di default (il guard fail-loud ha funzionato come designed) e su
   hardware reale vecchio. Le 2M sono baseline long-mode ovunque: niente
   CPUID, niente flag QEMU, split VGA/27.3 piu' facile. Tetto statico 64G
   fail-loud oltre (config test ≤ 32G).
4. **`R_X86_64_32` non contiene VMA alte.** Lo stub usa alias LMA valutati
   dal linker (`BOOT_PML4_LMA = BOOT_PML4 − OFFSET`) + LMA di `low_entry`
   da EIP reale (`call/pop` + delta). Questo rust-lld non accetta `ASSERT`
   (neanche `ASSERT(1, "x")`): l'invariante resta su gate-0, non sul linker.
5. **`cargo` non rilinkava su `linker.ld`.** Aggiunto `rerun-if-changed`
   in `kernel/build.rs` (stale silenzioso osservato: gate-0 rosso a VMA
   vecchie).

## Conseguenze

- Il basso canonico e' libero dopo `unmap_low()`; i PML4 user (creati dopo)
  ereditano il PML4 pulito per copia — nessun walk sui vivi.
- Le tabelle LOW restano nell'ELF (servono a OGNI boot per la transizione):
  27.3 pulisce la mappa runtime, non i dati.
- VGA in direct map con pagina UC dedicata (PAT di reset: PCD|PWT):
  corretto su HW reale, invisibile su QEMU.
- Limiti noti: direct map statica 64G fail-loud oltre; tabelle base
  vincolate a 0x90000–0x9FC00 (28K usati su 63K: margine per ~8 tabelle).

## Verifica

Gate invariato + `BOOT_OK` con `[boot] low unmapped`; selftest (build
dedicata) asserisce `PML4[0] == 0` e certifica il NULL-#PF (run congelata
nell'handler per disegno, log-check). Shell 30/30.

## Scoperte in corso d'opera (27.3)

6. **La scratch dei test collideva con le tabelle (fault ritardato).**
   `MAP_TEST_PHYS` era a 16M — la stessa LMA scelta per `.tables_high`:
   t12 scriveva pattern sopra le PD direct e il fault arrivava dopo (in
   `pop_msg`, heap illeggibile). Spostata a 64M + invariante di
   non-sovrapposizione verificata dal compilatore (`const assert` in
   `phys_mem.rs`: chi sposta una delle due e rompe l'altra non compila).
   Lezione: ogni indirizzo fisico statico va difeso da assert incrociati,
   non solo da commenti.
7. **Il thread di boot non riprende dopo il primo tick (pre-esistente).**
   Tutto il codice post-`BOOT_OK` in `rust_main` (Welcome, `selftests()`,
   halt loop) non esegue mai: la feature `selftest` era marcita in silenzio
   (compilava, non girava) e Welcome non e' mai stata mostrata. Verificato
   con build 27.2+serial (worktree): stesso silenzio. La prova 27.3 vive percio'
   pre-preemption (`selftest_low_unmap()` dopo `interrupts::init`). Il ciclo
   vita del thread di boot e' un follow-up scheduler, fuori 27.3.
8. **Un flake Test-4 FAT isolato.** Una run 27.3 ha fallito `testfat` Test 4
   (overwrite 8B: write ≠ 8, poi read `n=0`), con Test 7 (PIO write 9K +
   read-back) PASS nello stesso boot e suite 43/43: PIO e FS sani, rerun
   verde 7/7. Singolo caso su TCG sotto carico host — watch item (bound di
   polling PIO / dinamiche cache sotto varianza TCG), non indagato in 27.3.
