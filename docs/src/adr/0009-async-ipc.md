# ADR-0009: IPC asincrono — request-id interno + reply implicita kernel-side

**Status**: Accepted (Fase 13, primo passo additivo)
**Data**: 2026-09-07

## Contesto

L'IPC di ADR-0008 e' **sincrono**: un client ha al piu' 1 richiesta in volo per
canale. `send(channel, ...)` blocca il mittente finche' il server non fa
`reply`. Questo basta per call/reply singole (RPC), ma:

- un client che vuole pipeline N richieste (es. piu' read FS in parallelo) non
  puo': ogni `send` lo blocca finche' il server non risponde;
- il modello non prepara un futuro `async/await` in libr (un runtime che sospende
  il task del chiamante invece di bloccare tutto il processo).

ADR-0008 aveva rimandato il request-id esplicito alla "fase async" per un
motivo ABI: esporlo come sesto registro di ritorno (r8) con l'ABI a tupla SysV
rompeva l'inlining del wrapper (`write a 0x0`). La Fase 13 (async) deve
introdurre la correlazione senza toccare l'ABI dei registri.

## Vincoli

- **Additivita'**: le primitive async si AGGIUNGONO; il percorso sincrono resta
  intatto (rete di sicurezza: suite 19/19 prima della fase, 21/21 dopo).
- Server esistenti (userfs/console/devfs) **non devono cambiare** per servire
  client async: sono single-threaded, fanno `recv` → elabora → `reply`.
- Il request-id non deve occupare registri di ritorno extra.

## Decisione

1. **`req_id` come CAMPO INTERNO del messaggio**, non registro di ritorno.
   `PendingMsg` (PCB) guadagna `req_id: i64`; il mittente lo assegna da un
   contatore per-processo (`Process.req_next`), sia in `send` sia in
   `send_async`. Encoding **signed**: `req_id >= 0` = richiesta,
   `req_id < 0` = risposta async a `-req_id`.

2. **`send_async(channel, tag, w0, w1)` (syscall 33)**: come `send` ma NON
   blocca il mittente. Il kernel prova ad accodare al peer con
   `MsgQueue::try_push` (nuovo, ritorna `false` se la coda piena): backpressure
   esplicita → `-1` senza consegnare nulla (niente frame persi in silenzio).
   Ritorna il `req_id` assegnato in `rax`.

3. **`recv_nonblock()` (syscall 34)**: stesso `pop` di `recv` ma coda vuota →
   `-1` subito (nessun `BlockedOnRecv`).

4. **Reply implicita kernel-side, trasparente ai server**: il server continua a
   chiamare `reply(tag, w0, w1)`. Il kernel, alla reply, guarda lo stato del
   TARGET:
   - `BlockedOnReply` (client sincrono bloccato in `send`) → percorso attuale
     (`reply_slot` + risveglio);
   - altrimenti (client async: `Ready`/`Running`/`BlockedOnRecv`) → accoda un
     `PendingMsg { req_id: -reply_req, tag, w0, w1 }` nella sua `msg_queue`
     (risveglio solo se era `BlockedOnRecv`).
   `recv` registra sia `reply_chan` sia `reply_req` (il req_id del messaggio
   correntemente elaborato). Niente nuova syscall `reply_to`.

5. **`recv` espone il segno**: per una richiesta (`req_id >= 0`) `rdi` porta il
   canale sorgente (invariato per i server); per una risposta async
   (`req_id < 0`) `rdi` porta il `req_id` negativo. Il client distingue dal
   segno; `libr` decodifica in `IpcMsg { channel, req_id, tag, w0, w1 }`.

6. **API libr**: `send_async` → `Result<i64, ()>` (req_id); `recv_poll()`
   (non bloccante); `wait_reply(req)` (recv bloccante finche' non arriva il
   messaggio con `req_id == req`).

7. **Vincoli del primo passo** (rilassabili in fasi successive, documentati):
   - no mix di richieste sync e async in volo per lo stesso processo;
   - risposte consumate **FIFO** (`wait_reply` non riordina: messaggio diverso
     dall'atteso → errore);
   - percorso FS: **1 operazione async in volo per processo** (`libr` guard
     `FS_PENDING`) perche' il formato dei frame nel ring SPSC non ha lunghezza
     payload esplicita (derivata da `ring_available`): piu' frame nel ring non
     sono delimitabili;
   - la reply async a un target con `msg_queue` piena si perde (log nel kernel):
     il client deve raccogliere entro la capacita' della coda (8 slot).

## Conseguenze

- Kernel: `process.rs` (campo + `try_push`), `sched_rt.rs` — l'unico scheduler,
  esposto come `crate::sched` (send/recv/reply async + `pop_msg` condiviso),
  `syscall.rs` (dispatch 33/34).
- libr: primitive async + demo FS `read_async`/`fs_collect`.
- Test: helper usertestcli modalita' server echo (MODE_SRV); usertests t20
  (FS async 1-in-volo) e t21 (IPC async: N-in-volo FIFO + backpressure).
  Suite **21/21** + shell 3/3.
- Futuro: un runtime `async/await` in libr potra' appoggiarsi a `send_async` +
  sospensione del task; il superamento dei vincoli (reply_to esplicita,
  riordino, frame con lunghezza) verra' valutato quando servira' davvero.
