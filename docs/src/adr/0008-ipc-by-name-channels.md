# ADR-0008: IPC per nome — registry + channel nel kernel

**Status**: Accepted (Fase 12, implementata e verificata: 19/19 + shell 3/3)
**Data**: 2026-09-07

## Contesto

L'IPC sincrono per PID (Fase 7, ADR-0005) accoppia i peer al numero di
processo:
- `libr` hardcoda `FS_SERVER_PID = 4` per ogni operazione FS;
- il kernel (`kbd_process`) inietta i scancode a `CONSOLE_PID`;
- i processi figli deducono il padre da `cfg.sender` (o hardcodano il pid 1
  nei test);
- un processo terminato lascia riferimenti per-PID in volo (msg_queue,
  reply_target), rendendo fragile il riuso del PID e la pulizia (Fase 3).

Obiettivo: un IPC indirizzato **per nome di servizio**, typesafe, con overhead
costante sul percorso dati, che **non dipenda dal PID** per indirizzare i peer.

## Decisione

1. **Registry nel kernel** (non un servizio ring 3): le tabelle vivono nel
   kernel, aggiornate atomicamente con `exit_current` (stesso lock dello
   scheduler). Un registry ring 3 renderebbe ogni lookup un round-trip IPC,
   richiederebbe bootstrap (chicken-and-egg) e non potrebbe mai morire.

2. **`enum Service` nel crate condiviso** `syscall-numbers`, `#[repr(u64)]`,
   discriminant = indice di slot. Niente stringhe nel kernel. Il set dei
   servizi di sistema e' fisso e piccolo (`Fs`, `Console`, `Devfs`, `Kbd`,
   ...): i processi che NON sono servizi (test, helper) non registrano nomi —
   usano il canale di nascita. I nomi dinamici si aggiungeranno in futuro con
   una seconda tabella, senza rompere nulla.

3. **Oggetto `Channel`** (pool statico nel kernel): indirezione
   `{ id, dest_pid, alive }`. I messaggi viaggiano per `channel_id`, mai per
   PID. Quando il servizio muore il kernel invalida i canali che lo puntano →
   il riuso del PID diventa sicuro (premessa della Fase 3).

4. **Reply implicita al messaggio corrente** (non request-id esplicito in
   Fase 12, IPC per nome):
   `recv` registra il canale sorgente del messaggio appena ricevuto
   (`reply_chan`); `reply(tag, w0, w1)` risponde sul peer di quel canale.
   Generalizza il fix 9.2.2 (rispondere al mittente del messaggio in
   elaborazione, non all'ultimo sender) ai canali: piu' client concorrenti su
   un server non si sovrascrivono. Un request-id **esplicito** (correlazione
   per richieste multiple in volo) e' rimandato alla Fase 13 (async), dove
   servira' davvero — e richiederebbe un registro di ritorno in piu' (r8),
   che con l'ABI a tupla SysV rompeva l'inlining (write a 0x0).

5. **Canale di nascita**: `spawn` crea una coppia di canali collegati (modello
   pipe/socketpair). Il padre riceve un handle; il figlio nasce con canale 0 =
   parent. Elimina il PID dall'IPC padre-figlio e permettera' in futuro di
   leggere l'exit code del figlio sul canale.

6. **Separazione dei livelli**: l'IPC (canali: "con chi parlo") e i file
   (userfs: "cosa leggo") restano strati distinti. I device come file
   (`/dev/...`) sono una convenzione di userfs sopra l'IPC, non un motivo per
   fondere i due namespace.

7. **kbd come driver userspace** (opzione c, realizzata in Fase 15,
   ADR-0011): `userkbd` (porte PS/2) + `usertty` (decode/echo); il kernel fa
   solo routing + EOI dell'IRQ1 e sveglia l'owner per nome. La decisione
   originaria (opzione a, driver kernel) e' superata.

8. **Binari per stringa**: `spawn` continua a prendere il nome del binario
   embedded per stringa (catalogo di binari), separato dal servizio-enum. In
   futuro i binari potranno essere esternati su disco e init reso
   configurabile. (Realizzato in Fase 21, ADR-0017: `spawn_image` da
   `/bin`+`/test`, solo init/disk/fs embedded.)

## Conseguenze

- Nuove syscall: `service_register`, `service_lookup`; le primitive IPC
  (send/recv/reply) cambiano ABI: indirizzamento per **channel** invece che
  per PID, e `reply` implicita al messaggio corrente (via `reply_chan`).
- `spawn` ritorna un channel (non un pid); il figlio ha il canale 0 = parent.
- Migrazione completa (nessun binario legacy): libr, init, console, devfs, fs,
  shell, test suite. Le demo storiche srv/cli (basate su PID dedotto) sono
  rimosse dal catalogo binari.
- La pulizia dei processi (Fase 3) si appoggia su: slot servizio liberato
  all'exit, canali invalidati alla morte di un endpoint.
