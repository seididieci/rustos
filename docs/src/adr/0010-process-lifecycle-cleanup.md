# ADR-0010: Process lifecycle — cleanup kernel-side, notifica exit, kill, slot a generazioni

**Status**: Implemented (Fase 14)
**Data**: 2026-09-08

## Contesto

L'IPC per nome (ADR-0008) e l'IPC async (ADR-0009) hanno chiuso il cerchio
dell'indirizzamento e della correlazione delle richieste, ma la **morte** dei
processi e' rimasta incompleta. Oggi `exit_current` marca `Terminated` ma:

- **non libera** lo stack kernel, lo slot TSS, il CR3/address space, le page
  table, i ring e l'heap del processo;
- i PID **non si riusano** (limiti strutturali osservati: `ready_by_prio` a 32
  bit → max 32 processi pronti; pool TSS a 32 slot con allocazione monotona;
  `Vec<Process>` mai compattato; canali `Some(Channel{alive:false})` mai rimossi
  dal pool di 128);
- il **parent non viene notificato** della morte del figlio: chi aspetta un
  servizio (es. init) non puo' sapere che e' caduto ne' riavviarlo.

Inoltre non esiste un `kill` kernel-side: un processo puo' terminare solo se
esce da solo.

Obiettivo: un lifecycle completo che (a) liberi davvero le risorse, (b) riusi
gli slot/PID in modo sicuro, (c) notifichi il parent, (d) permetta al kernel
di terminare processi, il tutto in modo coerente con un'architettura dove il
kernel sa gia' tutto (canali, servizi, CBS) e nessuno fa wait/reap esplicito.

## Vincoli

- Non si puo' liberare lo stack del processo morente **mentre si gira ancora
  su di esso**: `exit_current` termina con uno `switch_to` sullo stack del
  morente stesso.
- Il riuso del PID deve mantenere validi i riferimenti esistenti (canali, slot
  servizio, CBS, `parent_chan`).
- La suite esistente (21/21) non deve regredire: l'exit volontario resta il
  percorso principale dei servizi e dei test.

## Decisione

1. **MODELLO 1 — cleanup kernel-side differito (non-POSIX)**. `exit_current`
   e `kill` marcano il processo `Terminated` e lo mettono in una **coda
   interna di reclaim**. Un passaggio di cleanup (a `on_tick` o subito dopo lo
   switch) esegue il teardown delle risorse. Il rilascio non avviene mai mentre
   si gira ancora sullo stack del morente. Nessun obbligo di `wait`/`reap` per
   il parent: la pulizia e' responsabilita' del kernel, coerente con il resto
   dell'architettura (canali, reply e CBS sono gia' kernel-side).

 2. **NOTIFICA EXIT UNIFICATA a tutti i peer** (estensione decisa in
    implementazione). Alla morte il kernel invia `EXIT_NOTIFY` (w0 = exit code,
    w1 = pid del morto) a **tutti i peer** dei canali del morente — parent,
    client e server — ciascuno sul canale che li collegava. Single path:
    `terminate` enumera le coppie `(peer, channel)` e le salva nel PCB
    (`die_peers`, max 31 peer distinti: bound provabile, nessuna policy di
    overflow); `reclaim_one` le notifica DOPO il teardown. Il parent resta un
    caso particolare di peer (la sua coppia porta il birth channel). I client
    sync bloccati in `send` sono sbloccati subito da `wake_senders`
    (complementare: non sono in `recv` e non possono ricevere la notifica);
    i client async in `wait_reply` ricevono `Err(ServerDied{pid,code})`.
    Consente a init di riavviare i servizi morti (restart effettivo: futuro).

 2b. **Semantica "UN peer e' morto"**. Il parent riceve le notifiche di TUTTI
    i figli, anche tardive (il reclaim gira al tick successivo): una notifica
    non riguarda necessariamente il server atteso. Chi conosce il pid atteso
    filtra per pid (t21/t24); chi conosce il canale filtra per canale
    (`wait_reply_chan`, usato da `fs_collect` sul canale FS cachato, stabile
    per vita del processo). Retry automatico e restart (init-restart)
    rimandati. Collisioni note e accettate: riuso channel-id tra terminate e
    reclaim (stesso hazard della notifica parent originaria; `peer()` valida
    la membership → mai misdelivery via reply); riuso PID del peer in finestra
    < 1 tick (chiusura completa con generazioni PID, punto 5, futuro).

3. **CASCATA**. La morte di un processo (exit/kill) termina **tutta la
   discendenza** (stesso percorso di cleanup, ricorsivo). Motivazione: in
   questo microkernel i figli sono parte del servizio gestito dal parent; la
   cascata evita orfani vivi con canali verso un morto. Morte di `init` →
   panic documentato (il root dei servizi non deve morire).

4. **KILL**. Nuova syscall `kill(pid)`: termina un processo per la stessa via
   di `exit` (cleanup + cascata + notifica). Il kill esplicito dell'intero
   sottoalbero e' rimandato alla fase "detach".

5. **SLOT A GENERAZIONI**. Allocatore di slot riusabile: un processo libero
   (Terminated e reclamato) torna disponibile; alla riallocazione il PID
   mantiene un valore di **generazione** incrementato, cosi' i riferimenti
   channels/CBS/servizi restano validi per la vita del processo e non c'e'
   confusione tra un processo nuovo e uno vecchio con lo stesso numero di slot.

6. **Detach (futuro, nota)**. In una fase successiva: un figlio che deve
   sopravvivere al parent (es. launcher/daemon) verra' "staccato" e
   ri-parentato a init; in quella fase si aggiungera' anche il kill esplicito
   del sottoalbero.

## Conseguenze

- Rilasciati a teardown: stack kernel (frame contigui), slot TSS, address space
  user (walk delle page table da `cr3`: foglie `owned` + page-table private),
  `HEAP_BRK`/`RING_PHYS`, slot canale.
- Nuova syscall `kill`; notifica exit unificata kernel→peer (EXIT_NOTIFY su
  ogni canale del morto, dopo il teardown); `libr` mantiene `exit` invariata
  e aggiunge `kill(pid)`, `wait_reply` con `WaitReplyError` (`ServerDied`) e
  `wait_reply_chan` per il filtro canale; nuovo servizio test-only
  `Service::Test` (slot usa-e-getta per i test di morte).
- Strutture toccate: `process.rs` (exit code/waiting_pid/tss_slot/die_peers),
  `sched_rt.rs` (coda reclaim, cleanup, kill, cascata, notifica unificata),
  `vmm_user.rs` (teardown address space, bit `owned`), `gdt.rs`
  (`free_tss_slot`), `phys_mem.rs` (free contigui), `channels.rs`
  (`enumerate_peers`, rimozione slot morti).
 - Fase 14 in AGENTS; test di churn oltre i limiti (t22), kill+notifica (t23),
   notifica unificata a tutti i peer (t24: path async/sync, slot liberato).
 - Responsabilita' dei server (14.11): ogni server purga il proprio stato
   per-canale su EXIT_NOTIFY. userfs (hub) rimuove rings/ftable/next_fd del
   morto, inoltra DEV_CLOSE ai driver best-effort e rimuove i mount del driver
   morto (stale first-match avvelenerebbe resolve_mount dopo re-registrazione).
   console/devfs non hanno stato per-client (hub-topology) → skip senza reply;
   se un driver avra' peer diretti con stato, ricavarne la tabella per
   (chan, fd). Test: t25 (morte driver + re-registrazione), t26 (morte client
   senza close + smoke completo).
 - Init-restart (14.12): init supervisiona console/fs/devfs (tabella
   bin/servizio/chan/pid + loop su EXIT_NOTIFY condiviso con l'attesa dei test;
   shell/uptime log-only). Respawn + attesa SVC_READY (fire-and-forget via
   send_async per tutti: una send sync resterebbe bloccata); backoff 20 tick +
   hold oltre 3 restart/300 tick. `service_pid` (36) per supervisione;
   retry libr uniform-retry-once con re-lookup bounded (~200 tick), caveat
   write at-least-once. Test: t27 (kill devfs → sparizione → ricomparsa →
   operativo). Rimandati: t28 (restart userfs: re-register driver +
   re-handshake client + reopen shell), generazioni PID.

## References

- ADR-0008 (IPC per nome / canale di nascita), ADR-0009 (IPC async).
- Modelli di riferimento: Linux process exit + subreaper; microkernel con
  cleanup kernel-side e riavvio servizi.
