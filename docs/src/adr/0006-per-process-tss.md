# ADR-0006: TSS per-processo con I/O permission bitmap

## Status

Accepted

## Context

Con il driver ATA PIO (Fase 9.2) un **processo userspace** (il fs server) deve
eseguire le istruzioni privilegiate `in`/`out` verso le porte 0x1F0-0x1F7. A
ring 3 l'accesso alle porte richiede un meccanismo di concessione:

- **IOPL** (bit 12-13 di RFLAGS): con IOPL >= 3 il processo puo' accedere a
  TUTTE le porte, e puo' anche eseguire `cli`/`sti`. Troppo permissivo.
- **TSS I/O permission bitmap**: per-porta. A CPL > IOPL la CPU consulta la
  bitmap del TSS corrente (`TR.base + IOMapBase + port/8`); bit = 1 → #GP,
  bit = 0 → accesso concesso. E' il meccanismo dei microkernel reali.

Il kernel usava finora **un singolo TSS** globale (solo `RSP0`, aggiornato a
ogni context switch). Con piu' driver futuri (VGA/CRTC, seriale, ...) serviva
una bitmap **per-processo**: ogni driver abilita solo le sue porte.

## Decision

**Ogni processo possiede un proprio `TaskStateSegment`** (ADR-0006) allocato in
un pool statico di 32 slot, con:

- `RSP0` = top dello stack kernel del processo (impostato UNA volta a
  creazione: niente piu' update per-switch della RSP0 globale)
- `IST[0]` = stack del double fault (comune)
- `I/O bitmap` da 8 KiB + terminatore `0xFF`: default tutte le porte bloccate;
  alla creazione vengono abilitate solo le porte in `io_ranges` del binario

Il context switch carica il TSS del processo con `ltr` (`load_process_tss`).

### Dettagli

- Pool: `static TSS_POOL: [TssWithIomap; 32]` (~260 KB), indirizzi statici →
  i descriptor GDT (32 system segment) sono pre-costruibili con
  `Descriptor::tss_segment_with_iomap` del crate `x86_64` (GDT a 69 entry).
- Slot 0 = TSS di boot/kernel (RSP0 = DF stack); i processi partono da slot 1.
- `NamedBinary`/manifest porta `io_ranges` per processo: `userdisk` →
  ATA PIO primario+secondario `[(0x1F0,0x1F7),(0x3F6,0x3F7),(0x170,0x177),
  (0x376,0x377)]`; `userkbd` → `[(0x60,0x64)]`; `userconsole` → CRTC cursore
  `[(0x3D4,0x3D5)]`; `userfs` → `&[]` (nessuna porta: qualunque `in/out` e'
  #GP, dalla Fase 16 il driver ATA vive in `userdisk`). I range dei servizi
  da disco sono dichiarati dallo spawner via `SpawnMeta` (Fase 21), senza
  toccare il kernel.
- **Bit busy**: `ltr` rifiuta un TSS gia' marcato busy (la CPU setta bit 41 del
  descriptor al primo load). Prima di ogni `ltr` il kernel azzera il bit busy
  del descriptor (via base GDT cacheata da `sgdt`).
- Nessun `cli`/`sti` concesso (a differenza di IOPL): solo le porte dichiarate.

### Alternative scartate

- **IOPL=3** (globale o per-processo): concede tutte le porte + cli/sti.
- **Bitmap unica + swap contenuti a ogni switch**: per-processo "di fatto" ma
  stato mutabile globale condiviso; fragile con piu' CPU.

## Consequences

- Ogni driver userspace dichiara le proprie porte I/O; il kernel non sa nulla
  dei driver.
- Costo: ~260 KB di statiche + `ltr` per-switch (trascurabile vs iretq+cr3).
- I processi senza `io_ranges` non possono toccare alcuna porta (bitmap tutta
  `0xFF` → #GP). Accesso a porta non consentita → `#GP` gestito.
- Il double fault resta funzionante: `IST[0]` e' copiato in ogni TSS.
- La bitmap e' globale al processo, non per-thread (single-core: irrilevante).
