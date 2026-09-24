# ADR-0034: history + editing di linea (disegno Fase 43b)

## Status

Accepted (implementazione in Fase 43b chiusa con questa ADR). Chiude la
Fase 43 (43a env in ADR-0033).

## Context

La shell aveva `read_line` minimale (solo backspace, Fase 18.0 con floor in
`usertty` via `line_len`); le frecce erano scartate in `usertty`
(`decode_bytes`, `_ => {}`). La vittoria 43b chiede Up/Down (history) e
Left/Right (cursore), con set completo (Home/End/Delete/Esc). Vincoli:

- L'echo dei digitati e' VGA-only (console), MAI seriale: gli assert di
  assenza dei test shell (`run_out`) si romperebbero altrimenti.
- La console sapeva `\r`, `\x08` (**che cancella**), `\n`, `\x0c`: nessuno
  spostamento cursore senza erase (serve per l'editing mid-line).
- Solo la shell consuma `/dev/input/keyboard` in modo interattivo (t31
  apre/chiude, nessuna digitazione).
- History dei COMANDI e' concetto shell (bash/readline), non terminale.

## Decision

1. **Readline nella shell (M2), tty raw.** `usertty` decodifica
   (frecce→`ESC[A/B/C/D`, Home/End→`ESC[H/F`, Delete→`ESC[3~`, Esc→`ESC`)
   e NON fa piu' echo (via `line_len` e il floor 18.0, che migra nella
   shell dove e' banale: backspace a buffer vuoto = no-op). La shell
   possiede buffer+cursore+history+echo console-only. Scartato: line
   discipline in tty (M1) — history a livello terminale (ogni riga letta,
   non solo comandi) nel posto sbagliato; e ibridi (echo diviso tty/shell)
   che non compongono per l'inserimento mid-line.
2. **Console: mini-parser ESC** (`ESC[D`/`ESC[C` cursore senza erase,
   `ESC[K` erase-to-EOL), stato persistente tra DEV_WRITE. Redraw shell
   senza conoscere il prompt: `ESC[D`×screen + `ESC[K` + buffer +
   `ESC[D`×(len-cur).
3. **History shell** (static, sessione): Enter accoda (non vuota, no duplicato
   consecutivo); Up/Down con `stash` della riga in corso; heredoc
   (`> `) non registrati. Solo ASCII; righe oltre 80 colonne non editabili
   (wrap VGA, documentato); Esc solitario con attesa bounded (mai hang).
4. **Test** `test-shell-43b.py` (9 check, digitazione reale via sendkey):
   assert sull'EFFETTO (output su seriale), mai sull'eco; helper
   `has_line`/`count_lines` in harness per gli anchor posizionali
   (`] X` vs `$ X` vs inizio slice — gli interleave kernel li spostano).

## Consequences

### Positive

- Up/Down/Left/Right/Home/End/Delete funzionanti, tty semplificato
  (trasporto raw), serial log invariato (echo sempre VGA-only).
- Zero kernel, zero syscall: gate 54/54 intatto per costruzione.

### Negative

- Bug vero trovato: `Us104Key` mappa Delete→`Unicode(0x7f)`, mai `RawKey`
  (stessa classe del fix Oem7 18.0) — l'arm `ESC[3~` non scattava mai
  (Delete muto mid-line). Override in `Us104Fix`.
- Bug vero trovato (test): gli anchor posizionali `] X\n` dipendevano dagli
  interleave kernel (prompt `$ ` incollato vs timestamp fresco) — helper
  `has_line`/`count_lines`, applicati anche a 7 assert pre-esistenti di
  41/42/43 (stessi check, piu' robusti).

### Neutral

- Contratto tty cambiato (niente echo: lo fa il lettore) — oggi solo la
  shell legge interattivo; documentato in ADR-0011.
