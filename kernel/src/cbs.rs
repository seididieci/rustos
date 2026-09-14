//! Constant Bandwidth Server (CBS) per lo scheduler RT (Fase 11.3).
//!
//! Ogni server CBS garantisce a un processo una quota di CPU: budget Q tick
//! ogni P tick. Il processo riceve SEMPRE i suoi Q tick anche sotto carico
//! al 100% (gli altri processi non possono consumare piu' di 1 - Σ BW).
//!
//! Pool statica di `MAX_CBS_SERVERS` slot, nessuna heap allocation. Sempre
//! compilato: lo scheduler RT unico di Velordor include il CBS.

use alloc::vec::Vec;
use spin::Mutex;

/// Numero massimo di server CBS contemporanei.
pub const MAX_CBS_SERVERS: usize = 8;

/// Capacita' massima combinata dei server CBS (~70%). Il resto della CPU
/// resta ai processi fixed-priority.
pub const CBS_BW_CAP: f64 = 0.70;

#[derive(Clone, Copy, Debug)]
pub struct CbsServer {
    /// Budget garantito per periodo (in tick di PIT, 1 tick = 10 ms).
    pub budget_ticks: u32,
    /// Periodo (in tick).
    pub period_ticks: u32,
    /// Budget residuo nel periodo corrente. Decrementato a ogni tick del
    /// processo associato. A 0 il processo viene throttled.
    pub remaining_budget: i32,
    /// Tick assoluto di fine periodo corrente.
    pub deadline: u64,
    /// Processo associato al server (`None` se nessun processo e' legato).
    pub task_pid: Option<usize>,
    /// Il server e' attivo (creato, non distrutto).
    pub active: bool,
    /// Bandwidth garantita = Q / P.
    pub bandwidth: f64,
}

/// Pool statica dei server CBS.
static CBS_POOL: Mutex<[Option<CbsServer>; MAX_CBS_SERVERS]> =
    Mutex::new([None; MAX_CBS_SERVERS]);

/// Crea un nuovo server CBS con i parametri dati. Esegue l'admission
/// control: la somma delle bandwidth di tutti i server attivi + la nuova
/// non deve superare `CBS_BW_CAP`. Ritorna l'id dello slot o `Err(())` se
/// rifiutato.
pub fn create(budget_ticks: u32, period_ticks: u32) -> Result<usize, ()> {
    if budget_ticks == 0 || period_ticks == 0 || budget_ticks > period_ticks {
        return Err(());
    }
    let new_bw = budget_ticks as f64 / period_ticks as f64;
    let mut pool = CBS_POOL.lock();

    // Admission control: somma bandwidth esistenti + nuovo.
    let mut total_bw = new_bw;
    for slot in pool.iter() {
        if let Some(s) = slot {
            if s.active {
                total_bw += s.bandwidth;
            }
        }
    }
    if total_bw > CBS_BW_CAP {
        return Err(());
    }

    // Trova slot libero.
    for (i, slot) in pool.iter_mut().enumerate() {
        if slot.is_none() {
            let now = crate::pit::ticks();
            *slot = Some(CbsServer {
                budget_ticks,
                period_ticks,
                remaining_budget: budget_ticks as i32,
                deadline: now + period_ticks as u64,
                task_pid: None,
                active: true,
                bandwidth: new_bw,
            });
            crate::serial_println!(
                "[cbs] created server {} Q={} P={} bw={:.2}%",
                i, budget_ticks, period_ticks, new_bw * 100.0
            );
            return Ok(i);
        }
    }
    Err(()) // nessuno slot libero
}

/// Lega un server CBS al processo `pid`.
pub fn attach(server_id: usize, pid: usize) -> Result<(), ()> {
    let mut pool = CBS_POOL.lock();
    if let Some(Some(s)) = pool.get_mut(server_id) {
        if s.active {
            s.task_pid = Some(pid);
            crate::serial_println!(
                "[cbs] attached server {} to pid {}",
                server_id, pid
            );
            return Ok(());
        }
    }
    Err(())
}

/// Informazioni di debug su un server CBS (bandwidth esclusa: userspace non
/// la legge — `cbs_get_info` ritorna solo budget/period/remaining).
pub struct CbsInfo {
    pub budget: u32,
    pub period: u32,
    pub remaining: i32,
}

/// Ritorna le informazioni di un server CBS (budget/period/remaining/bw).
pub fn get_info(server_id: usize) -> Option<CbsInfo> {
    let pool = CBS_POOL.lock();
    if let Some(Some(s)) = pool.get(server_id) {
        Some(CbsInfo {
            budget: s.budget_ticks,
            period: s.period_ticks,
            remaining: s.remaining_budget,
        })
    } else {
        None
    }
}

/// Rilascia tutti i server CBS legati al processo `pid` (da chiamare quando
/// il processo esce o viene killato). Lo slot torna libero (None) cosi' un
/// nuovo server puo' riusarlo (Fase 14): prima veniva solo marcato `inactive`
/// e saturava il pool di `MAX_CBS_SERVERS`. Senza questo rilascio un server
/// rimarrebbe con `task_pid` che punta a un processo Terminated:
/// `tick_replenish` lo risveglierebbe a ogni deadline → `set_ready` su un
/// processo morto → il scheduler lo riprende dentro il `loop { hlt() }` di
/// `exit_current` con IF=0 → congelamento totale del sistema.
pub fn release_pid(pid: usize) {
    let mut pool = CBS_POOL.lock();
    for slot in pool.iter_mut() {
        if let Some(s) = slot {
            if s.active && s.task_pid == Some(pid) {
                *slot = None;
            }
        }
    }
}

/// Contabilita' budget: decrementa il budget del server associato al
/// processo `pid`. Ritorna `true` se il budget e' esaurito (throttled).
///
/// Chiamato da `sched_rt::on_tick` a ogni tick del processo corrente.
pub fn tick_budget(pid: usize) -> bool {
    let mut pool = CBS_POOL.lock();
    for slot in pool.iter_mut() {
        if let Some(s) = slot {
            if s.active && s.task_pid == Some(pid) {
                s.remaining_budget -= 1;
                return s.remaining_budget <= 0;
            }
        }
    }
    false // nessun server per questo PID
}

/// Replenishment: controlla se qualche server ha la deadline scaduta e
/// resetta il budget. Ritorna la lista dei PID da rimettere in ready queue.
///
/// Chiamato da `sched_rt::on_tick` PRIMA del decrement budget, cosi' un
/// processo throttled ha la possibilita' di tornare schedulabile nello
/// stesso tick in cui viene riapprovvigionato.
pub fn tick_replenish() -> Vec<usize> {
    let mut pool = CBS_POOL.lock();
    let now = crate::pit::ticks();
    let mut to_wake = Vec::new();

    for slot in pool.iter_mut() {
        if let Some(s) = slot {
            if s.active && now >= s.deadline {
                // Replenishment: budget = Q, deadline += P.
                s.remaining_budget = s.budget_ticks as i32;
                s.deadline += s.period_ticks as u64;
                if let Some(pid) = s.task_pid {
                    to_wake.push(pid);
                }
            }
        }
    }
    to_wake
}
