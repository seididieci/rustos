# ADR-0001: Use Rust Nightly

## Status

Accepted

## Context

Lo sviluppo di un kernel bare-metal richiede feature che non sono ancora stabilizzate nel compiler Rust stable. Le principali necessità sono:

1. **`alloc` crate**: necessario per usare `Box<T>`, `Vec<T>`, `String` senza `std`
2. **`build-std`**: per riconpilare `core` e `alloc` per il target personalizzato `x86_64-unknown-none`
3. **`naked_functions`**: per il context switch assembly (registri controllo totali)
4. **Custom target JSON**: per definire un target bare-metal personalizzato

Senza nightly, è impossibile compilare un kernel Rust per bare-metal.

## Decision

Usare Rust nightly come toolchain principale per il progetto. Il file `rust-toolchain.toml` pinna nightly nella root del workspace.

```toml
[toolchain]
channel = "nightly"
components = ["llvm-tools-preview", "rust-src"]
targets = ["x86_64-unknown-none"]
```

## Consequences

### Positive

- Accesso a tutte le feature necessarie per il kernel
- `build-std` permette di riconpilare `core` per il nostro target
- Supporto completo per custom targets JSON
- Aggiornamenti frequenti con bugfix e nuove feature

### Negative

- Le nightly releases possono introdurre breaking changes (raro ma possibile)
- Alcune crate potrebbero non essere immediatamente compatibili con le nightly
- È necessario gestire manualmente gli aggiornamenti con `rustup update`

### Neutral

- Il kernel Linux stesso usa Rust nightly per i moduli, quindi è un approccio consolidato
- Il file `rust-toolchain.toml` assicura che tutti i contributor usino la stessa versione

## Alternatives Considered

- **Stable Rust + C/Assembly**: Avrebbe richiesto di riscrivere in C molte parti, perdendo i vantaggi di Rust (ownership, borrow checker). Inoltre, il blocco fondamentale è `build-std` che è solo nightly.

- **Solo stable senza `alloc`**: Impossibile. Senza `alloc`, non è possibile usare strutture dati dinamiche (`Vec`, `Box`, `String`) che sono essenziali per un kernel.

- **Usare i pacchetti Fedora**: I pacchetti Fedora forniscono solo stable Rust e non includono i componenti necessari (`rust-src`, `llvm-tools-preview`).

## References

- [Rust Nightly Documentation](https://doc.rust-lang.org/book/appendix-07-nightly-rust.html)
- [build-std Documentation](https://doc.rust-lang.org/cargo/reference/unstable.html#build-std)
- [Rust for Linux](https://rust-for-linux.com/) - Il kernel Linux usa Rust nightly
