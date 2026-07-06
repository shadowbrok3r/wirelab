//! Flash the WireLab firmware onto a connected board from the GUI:
//! `cargo build` for the board's chip, then `espflash flash` on the port,
//! all on a background thread with output streamed to the console.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};

use wirelab_proto::ChipKind;

enum Msg {
    Line(String),
    Done(bool),
}

#[derive(Default)]
pub struct FlashState {
    rx: Option<Receiver<Msg>>,
}

/// Build features / target / artifact path per chip.
fn chip_build(chip: ChipKind) -> Option<(Vec<&'static str>, &'static str)> {
    match chip {
        ChipKind::Esp32C3 => Some((vec![], "riscv32imc-unknown-none-elf")),
        ChipKind::Esp32C5 => Some((
            vec!["--no-default-features", "--features", "esp32c5,wifi"],
            "riscv32imac-unknown-none-elf",
        )),
        _ => None,
    }
}

fn firmware_dir() -> Option<PathBuf> {
    let candidates = [
        PathBuf::from("firmware/wirelab-fw"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../firmware/wirelab-fw"),
    ];
    candidates.into_iter().find(|c| c.join("Cargo.toml").exists())
}

impl FlashState {
    pub fn running(&self) -> bool {
        self.rx.is_some()
    }

    /// Kick off build + flash; refuses when unsupported or already running.
    pub fn start(&mut self, chip: ChipKind, port: &str, log: &mut Vec<String>) {
        if self.running() {
            log.push("flash already in progress".into());
            return;
        }
        let Some((features, target)) = chip_build(chip) else {
            log.push(format!(
                "flashing {} from the GUI isn't wired up (C3 and C5 are)",
                chip.name()
            ));
            return;
        };
        let Some(dir) = firmware_dir() else {
            log.push("firmware/wirelab-fw not found next to the app".into());
            return;
        };
        // Absolute paths: espflash runs with `dir` as its cwd, so a relative
        // ELF path would resolve against the wrong base.
        let dir = match dir.canonicalize() {
            Ok(d) => d,
            Err(e) => {
                log.push(format!("firmware dir: {e}"));
                return;
            }
        };
        let elf = dir.join(format!("target/{target}/release/wirelab-fw"));
        let port = port.to_string();
        let (tx, rx) = channel();
        self.rx = Some(rx);
        log.push(format!("flash: building firmware for {}…", chip.name()));

        std::thread::Builder::new()
            .name("wirelab-flash".into())
            .spawn(move || {
                let mut build = Command::new("cargo");
                build
                    .arg("build")
                    .arg("--release")
                    .args(&features)
                    // The firmware's default target is the C3's; be explicit.
                    .arg("--target")
                    .arg(target)
                    .current_dir(&dir);
                if !run_streamed(build, &tx) {
                    let _ = tx.send(Msg::Done(false));
                    return;
                }
                let _ = tx.send(Msg::Line(format!("flash: espflash → {port}")));
                let mut flash = Command::new("espflash");
                flash
                    .arg("flash")
                    .arg("--non-interactive")
                    .arg("--port")
                    .arg(&port)
                    .arg(&elf)
                    .current_dir(&dir);
                let ok = run_streamed(flash, &tx);
                let _ = tx.send(Msg::Done(ok));
            })
            .expect("spawn flash thread");
    }

    /// Drain background output into the console; true while still running.
    pub fn poll(&mut self, log: &mut Vec<String>) -> bool {
        let Some(rx) = &self.rx else { return false };
        let mut done = None;
        for msg in rx.try_iter() {
            match msg {
                Msg::Line(l) => log.push(format!("  {l}")),
                Msg::Done(ok) => done = Some(ok),
            }
        }
        if let Some(ok) = done {
            self.rx = None;
            log.push(if ok {
                "flash: done — click Connect (the board reboots into the new firmware)".into()
            } else {
                "flash: FAILED — see output above".into()
            });
            return false;
        }
        true
    }
}

/// Run a command, forwarding interesting stdout/stderr lines.
fn run_streamed(mut cmd: Command, tx: &Sender<Msg>) -> bool {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(Msg::Line(format!("failed to start {:?}: {e}", cmd.get_program())));
            return false;
        }
    };
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        readers.push(std::thread::spawn({
            let tx = tx.clone();
            move || forward(BufReader::new(out), &tx)
        }));
    }
    if let Some(err) = child.stderr.take() {
        readers.push(std::thread::spawn({
            let tx = tx.clone();
            move || forward(BufReader::new(err), &tx)
        }));
    }
    for r in readers {
        let _ = r.join();
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

fn forward<R: BufRead>(reader: R, tx: &Sender<Msg>) {
    for line in reader.lines().map_while(Result::ok) {
        let l = line.trim_end();
        // Keep the console readable: skip cargo's per-crate chatter.
        if l.is_empty() || l.trim_start().starts_with("Compiling") {
            continue;
        }
        let _ = tx.send(Msg::Line(l.to_string()));
    }
}
