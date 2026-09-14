# IPC (Inter-Process Communication)

> ⭐ **Capitolo centrale dell'architettura microkernel** (ADR-0005).
> Fase 7 — **implementata e verificata**. Aggiornato in **Fase 12** (ADR-0008):
> IPC per nome con **registry + channel nel kernel** (vedi sezione in fondo).

## Panoramica

Comunicazione **sincrona registro-based**, stile seL4/L4. Il messaggio viaggia nei
registri della CPU (nessun buffer/zero-copy in questa fase): il kernel fa **solo** da
smistatore tra i PCB, senza copiare payload.

Dal **Fase 12** l'indirizzamento e' per **canale** (channel id), non per PID:
vedi [sezione dedicata](#fase-13-ipc-per-nome--registry--channel).

```
Processo A (server)                Processo B (client)
     │                                  │
     │   recv() ── blocca               │
     │                                  │ send(dest=A, tag, w0, w1)
     │ ◄────────────────────────────────┤
     │   (kernel la sveglia e le passa  │ (kernel blocca B)
     │    il messaggio nei registri)    │
     │ elabora                          │ bloccato in attesa risposta
     │ reply(tag, w0', w1') ───────────►│
     │                              sbloccato con la risposta in rdi/rsi/rdx/r10
```

Perché sincrona: **zero** logica/queue da gestire nel kernel oltre al blocco/sblocco,
messaggio = registri CPU, semantica call/reply naturale per client/server (RPC).

## Primitive e numeri di syscall

| Primitive | Numero | Semantica |
|-----------|--------|-----------|
| `send(dest, tag, w0, w1)` | 16 | consegna il messaggio a `dest` e **blocca** finché `dest` non fa `reply` |
| `recv()` | 17 | **blocca** finché non arriva un messaggio, poi lo restituisce |
| `reply(tag, w0, w1)` | 18 | risponde al mittente in attesa e lo **sblocca** |
| `send_async(channel, tag, w0, w1)` | 33 | come `send` ma **non blocca**: ritorna il `req_id` (>= 1) o -1 (Fase 13) |
| `recv_nonblock()` | 34 | come `recv` ma ritorna -1 subito se la coda è vuota (Fase 13) |

ABI: `rax`=numero, `rdi/rsi/rdx/r10`=arg1-4; il risultato torna in `rax` **più**
`rdi/rsi/rdx/r10` (multi-parola). L'entry `syscall_entry` implementa il ritorno
multi-register via `ipc_override`: a `sysret`, se `ipc_override != 0`, svuota
`rdi/rsi/rdx/r10` con i valori `ret_rdi/rsi/rdx/r10` (v. [`06-syscalls.md`](./06-syscalls.md)).

### Valori di ritorno

| Operazione | `rax` | registri extra |
|-----------|-------|----------------|
| `send` ok | 0 | `rsi=reply.tag, rdx=reply.w0, r10=reply.w1` |
| `send` err (dest inesistente / nessuna reply possibile) | -1 | `rdi/rsi/rdx/r10=0` |
| `recv` ok (richiesta) | 0 | `rdi=channel, rsi=tag, rdx=w0, r10=w1` |
| `recv` ok (risposta async) | 0 | `rdi=req_id negativo, rsi=tag, rdx=w0, r10=w1` |
| `recv` err (nessun altro processo) | -1 | `rdi/rsi/rdx/r10=0` |
| `reply` ok | 0 | — |
| `reply` err (nessun messaggio in elaborazione) | -1 | — |
| `send_async` ok | req_id (>= 1) | — |
| `send_async` err (coda peer piena / canale morto) | -1 | — |
| `recv_nonblock` ok | 0 | come `recv` |
| `recv_nonblock` err (coda vuota) | -1 | — |

## Implementazione kernel

Tutto lo stato IPC vive nel **PCB** del processo (`kernel/src/process.rs`), *non* in
`PERCPU` (zona transitoria single-slot condivisa):

- `PendingMsg { sender, tag, w0, w1 }` → coda `msg_queue` del ricevente.
- `PendingReply { tag, w0, w1 }` → `reply_slot` del mittente in attesa.
- `waiting_sender: Option<usize>` → a chi il processo "deve" la reply.
- `IpcState { None, BlockedOnRecv, BlockedOnReply }` → perché il processo è bloccato.

### send

```rust
pub fn ipc_send(dest: usize, tag: u64, w0: u64, w1: u64) -> IpcResult {
    // 1. accoda PendingMsg{sender=cur, tag, w0, w1} a dest.msg_queue
    // 2. se dest era in BlockedOnRecv → ipc_state=None, state=Ready (sveglia)
    // 3. segna dest.waiting_sender = Some(cur)
    // 4. blocca cur (BlockedOnReply)
    // 5. switch al prossimo pronto; al risveglio legge cur.reply_slot
}
```

### recv

```rust
pub fn ipc_recv() -> IpcResult {
    // 1. se msg_queue non vuota → return subito (sender, tag, w0, w1)
    // 2. altrimenti blocca (BlockedOnRecv); al risveglio loop (→ 1)
}
```

### reply

```rust
pub fn ipc_reply(tag: u64, w0: u64, w1: u64) -> IpcResult {
    // 1. target = cur.waiting_sender (None → -1)
    // 2. target.reply_slot = Some(PendingReply{tag, w0, w1})
    // 3. target: ipc_state=None, state=Ready (sveglia il mittente)
}
```

Interfaccia `IpcResult { rax, rdi, rsi, rdx, r10 }`; `apply_ipc` (in `syscall.rs`) la
propaga ai registri di ritorno della syscall impostando `ipc_override`.

## Demo storica (Fase 7)

Nella Fase 7 due processi user (ring 3) dimostravano il modello: `usersrv` →
`usercli` (client che assume `server_pid = getpid() - 1`). Queste demo
(`testland/srv`, `testland/cli`) NON sono piu' buildate dalla Fase 12: basate
sull'IPC per PID dedotto, sono state rimosse dal catalogo binari (il modello
attuale e' a canali di nascita) e i sorgenti cancellati in un batch di igiene.

## Note sul bug `swapgs` (perché l'entry non usa GS)

La primitiva bloccante (`send`/`recv`) **non** torna mai via `sysret` subito: fa un
context switch con lo stato GS *già* scambiato dall'entry. In passato l'entry usava
`swapgs` per indirizzare `PERCPU` via `GS`; il conteggio degli `swapgs` per-CPU
divergeva da quello per-processo → la syscall successiva di un altro processo
invertiva lo stato → `GS.base=0` nel handler → `mov gs:0x18,rsp` scriveva nel vuoto →
`user_rsp` stale → `sysret` con stack sbagliato → salto a `rip=0`.

Soluzione adottata (Fase 7): **niente `swapgs`** — l'entry accede a `PERCPU` e allo
stato IPC **`rip`-relative** e via PCB (per-processo), che resta valido attraverso i
context switch. L'`user_rsp`/`user_r12` vengono copiati sullo **stack kernel
per-processo** (non tenuti in `PERCPU`, che è condiviso e verrebbe sovrascritto da un
altro processo mentre il mittente è bloccato).

## Fase 12 — IPC per nome: registry + channel (ADR-0008)

L'IPC sincrono per PID (sopra) accoppiava i peer al numero di processo. Dalla
Fase 12 il kernel espone un **registry di servizi** e indirizza i messaggi per
**channel**:

- **`enum Service`** nel crate `syscall-numbers` (`Fs`, `Console`, `Devfs`,
  `Init`): ogni servizio di sistema occupa uno slot (tabella nel kernel,
  `channels.rs`). `service_register(service)` (31) lo occupa;
  `service_lookup(service)` (32) risolve il nome in un canale verso l'owner.
- **`Channel`**: coppia bidirezionale tra due processi. `spawn` crea il
  **canale di nascita** (il figlio lo ha come canale 0 = parent, il parent
  riceve l'handle). La morte di un endpoint invalida i canali che lo
  coinvolgono e libera lo slot servizio di cui era owner.
- **send/recv/reply per canale**: `send(channel, tag, w0, w1)` (16, canale 0 =
  parent), `recv()` (17, ritorna channel sorgente + tag/w0/w1), `reply(tag, w0,
  w1)` (18). La reply e' **implicita al messaggio corrente**: `recv` registra
  il canale sorgente in `reply_chan`, `reply` risponde sul peer di quel canale
  (fix 9.2.2 generalizzato). Niente request-id esplicito lato server in Fase 12
  (ABI a 6 registri non inlinabile): la Fase 13 lo introduce come campo interno
  del messaggio, senza toccare i registri di ritorno.
- **Migrazione**: fs/console/devfs si registrano per nome; `libr` risolve `Fs`
  per nome (`fs_chan`); kbd (kernel) risolve `Console` per nome; init
  sincronizza il boot attendendo l'ACK "Fs pronto" da userfs. Le demo storiche
  srv/cli (basate su PID dedotto) sono state rimosse dal catalogo binari.

Vedi [ADR-0008](./adr/0008-ipc-by-name-channels.md) per la decisione completa.

## Fase 13 — IPC asincrono (primo passo, additivo)

Motivazione: l'IPC di Fase 12 e' **sincrono** — un client ha al piu' 1 richiesta
in volo per canale (si blocca in `send` finche' il server non fa `reply`).
La Fase 13 aggiunge primitive **async** (syscall 33/34) per avere piu' richieste
in volo, mantenendo **intatto** il percorso sincrono (rete di sicurezza).

### Design

- **`req_id` interno al messaggio** (`PendingMsg.req_id`, assegnato dal mittente
  via `req_next`). Encoding **signed**: `req_id >= 0` = richiesta; `req_id < 0`
  = risposta async a `-req_id`. Il segno si legge in `recv`.
- **Reply implicita kernel-side**: il server continua a usare `reply()` (18)
  senza sapere nulla di async. Il kernel, alla reply, guarda lo stato del
  target: se e' `BlockedOnReply` (client sincrono bloccato in `send`) →
  comportamento attuale (`reply_slot`); se non e' bloccato (client async) →
  accoda un messaggio-risposta con `req_id = -reply_req` nella sua `msg_queue`.
  Trasparente ai server (userfs/console/devfs non cambiano).
- **`send_async`** (33): come `send` ma il mittente **non si blocca**: il kernel
  prova ad accodare al peer (`try_push`); coda piena (backpressure) o canale
  morto → -1 senza consegnare nulla. Ritorna il `req_id` assegnato.
- **`recv_nonblock`** (34): come `recv` ma coda vuota → -1 subito.
- **Il client raccoglie** con `wait_reply(req_id)` in `libr`: `recv` bloccante
  finche' non arriva il messaggio con `req_id == req_id`.

### Vincoli del primo passo (rilassabili in futuro)

- **No mix sync/async in volo per lo stesso processo**: chi usa `send_async`
  raccoglie con `wait_reply`/`recv` prima di un'eventuale `send` sincrona.
- **Risposte consumate FIFO**: il server e' single-threaded e risponde in
  ordine di `recv` → le reply arrivano nell'ordine delle richieste.
  `wait_reply` non riordina: un messaggio diverso da quello atteso → errore.
- **FS async = 1 operazione in volo per processo**: il formato dei frame nel
  ring SPSC non ha lunghezza payload esplicita (derivata da `ring_available`) →
  un solo frame nel ring alla volta. `libr` espone `read_async`/`fs_collect`
  con un guard (`FS_PENDING`) che rifiuta ogni altra op FS finche' non si
  raccoglie. La risposta FS async e' un ack + frame nel response ring.
- **Reply async persa se la coda del target e' piena** (limitazione nota: il
  client deve raccogliere entro la capacita' della `msg_queue`, 8 slot).

### Esempio (IPC puro)

```rust
let req = libr::send_async(chan, T_REQ, 42, 0)?;   // non blocca
// ... altro lavoro ...
let m = libr::wait_reply(req)?;                    // blocca finche' arriva
// m.req_id == req, m.w0 = risposta del server
```

### Regressione

- kernel: `PendingMsg.req_id`, `Process.req_next`/`reply_req`,
  `MsgQueue::try_push`, `ipc_send_async`/`ipc_recv_nonblock`, reply async in
  `ipc_reply` (in `sched_rt.rs`, esposto come `crate::sched`).
- userland: usertestcli modalita' "server echo" (MODE_SRV) per i test;
  usertests t20 (FS async) e t21 (IPC async + backpressure).
- **FS async generalizzato nonbloccante** (Fase 15, per driver-server come
  `usertty`): `fs_op_async` (tag IPC parametrico: `FS_NOTIFY` per le op,
  `FS_REGISTER` per la registrazione), `write_async`/`open_async`/
  `fs_register_async`/`fs_buf_reg_async`, `fs_collect_msg` (collect su
  messaggio gia' ricevuto via poll, mai bloccante), `fs_abort_pending`.
  Regola: un server che risponde a relay sincrone non emette mai IPC FS
  sincrone (ciclo userfs↔driver), dorme in `recv()` e si sveglia su
  notify/relay/reply (event-driven). Dettagli in
  [ADR-0011](./adr/0011-userspace-keyboard-terminal.md).

## Fase 14 — notifica unificata di morte + `wait_reply` con errore (ADR-0010)

Quando un processo muore, **tutti i peer** dei suoi canali ricevono
`EXIT_NOTIFY` (`w0` = exit code, `w1` = pid del morto) sul canale che li
collegava — non solo il parent. Single path: `terminate` enumera le coppie
`(peer, channel)` e le salva nel PCB (`die_peers`, max 31 peer distinti:
bound provabile); `reclaim_one` le notifica DOPO il teardown fisico.

- **Client sync** bloccati in `send` verso il morto: sbloccati subito da
  `wake_senders` con errore (meccanismo invariato, complementare).
- **Client async** in `wait_reply`: la reply non arrivera' mai → `wait_reply`
  ritorna `Err(WaitReplyError::ServerDied { pid, code })` invece di attendere
  per sempre. Chi conosce il pid atteso filtra per pid (t21/t24); chi conosce
  il canale filtra per canale (`wait_reply_chan`, usato da `fs_collect` sul
  canale FS cachato).
- **Semantica "UN peer e' morto"**: il parent riceve le notifiche di TUTTI i
  figli, anche tardive (il reclaim gira al tick successivo). Una notifica
  stale non riguarda necessariamente il server atteso: confrontare `pid` (o
  canale) prima di concludere. Retry automatico e restart dei server
  (init-restart) sono lavoro futuro documentato.
- **Mai rispondere a `EXIT_NOTIFY`** (`drain_stray` la scarta senza reply):
  il mittente e' morto e non c'e' nessuno a leggere la risposta.
- **Retry client su server morto** (Fase 14, init-restart): `libr::fs_send`
  invalida il canale cachato alla prima send fallita, ri-risolve per nome
  (bounded ~200 tick: attende un eventuale restart) e ritenta UNA volta sola.
  Caveat write at-least-once documentato. `service_pid(service)` (syscall 36)
  espone il pid owner per supervisione/diagnostica.
- **Cleanup per-peer nei server** (Fase 14.11): ogni server purga il proprio
  stato per-canale alla morte del peer. userfs (l'hub: tutto il traffico
  passa da lui) rimuove `rings[chan]`, tutti gli fd di `ftable` per quel
  canale (inoltra `DEV_CLOSE` ai driver best-effort, così restano puliti
  anche loro) e i mount il cui `driver_chan` è morto (altrimenti lo stale,
  primo in lista per `resolve_mount`, avvelenerebbe il routing anche dopo
  re-registrazione). console/devfs non hanno stato per-client (tabella fd
  globale in devfs, buffer unico in console: tutto il traffico arriva
  multiplexato dall'unico canale userfs↔driver) → skip esplicito senza reply.
  Se in futuro un driver avrà peer diretti con stato per-client, ricavarne
  la tabella per `(chan, fd)` e purgarla come userfs.

## Riferimenti

- [seL4 — IPC](https://sel4.systems/)
- [seL4 Reference Manual — IPC](https://docs.sel4.systems/projects/sel4-manual/latest/ipc.html)
- [OSDev Wiki — Inter Process Communication](https://wiki.osdev.org/Inter_Process_Communication)
