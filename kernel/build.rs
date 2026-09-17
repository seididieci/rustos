use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let boot_src = manifest_dir.join("src/boot.asm");
    let boot_obj = out_dir.join("boot.o");

    println!("cargo:rerun-if-changed={}", boot_src.display());
    // Il linker script determina VMA/LMA dell'immagine: senza questa riga
    // modificarlo non invalida la build (stale silenzioso, osservato in H1).
    println!("cargo:rerun-if-changed={}", manifest_dir.join("linker.ld").display());

    // I binari user embeddati (user_binary.rs) vengono rigenerati da
    // scripts/build-userland.sh e scripts/build-tests.sh: se cambiano il
    // kernel va ricompilato. Le dir (add/remove di un .bin) + ogni .bin
    // presente (cambio contenuto): senza i per-file, modificare un binario
    // esistente non invaliderebbe la build (stale silenzioso, osservato).
    for dir in ["../userland/build", "../testland/build"] {
        let d = manifest_dir.join(dir);
        println!("cargo:rerun-if-changed={}", d.display());
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().map_or(false, |x| x == "bin") {
                    println!("cargo:rerun-if-changed={}", p.display());
                }
            }
        }
    }

    let status = Command::new("nasm")
        .args(["-f", "elf64"])
        .arg(&boot_src)
        .arg("-o")
        .arg(&boot_obj)
        .status()
        .expect("failed to run nasm for boot.asm");

    if !status.success() {
        panic!("nasm failed to assemble boot.asm");
    }

    // boot.o
    println!("cargo:rustc-link-arg-bins={}", boot_obj.display());

    // Link del kernel: -no-pie e il linker script sono specifici del KERNEL e
    // vanno emessi QUI (via link-arg del package), NON nelle rustflags della
    // root .cargo/config.toml. Se fossero nella config di root, si
    // infiltrerebbero (rustflags additive via discovery) anche nel link dei
    // binari userspace costruiti nella stessa radice.
    println!("cargo:rustc-link-arg-bins=-no-pie");
    let ld = manifest_dir.join("linker.ld");
    println!("cargo:rustc-link-arg-bins=-T{}", ld.display());
}
