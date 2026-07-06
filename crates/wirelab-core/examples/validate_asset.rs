//! Validate one board or component JSON: `cargo run -p wirelab-core --example validate_asset -- <file>`

use wirelab_core::board::BoardProfile;
use wirelab_core::component::ComponentDef;
use wirelab_core::library::{lint_board, lint_component};

fn main() {
    let path = std::env::args().nth(1).expect("usage: validate_asset <file.json>");
    let text = std::fs::read_to_string(&path).expect("read file");
    let board = serde_json::from_str::<BoardProfile>(&text);
    let comp = serde_json::from_str::<ComponentDef>(&text);
    let problems = match (board, comp) {
        (Ok(b), _) => {
            println!("parsed as board profile '{}' ({} pins)", b.id, b.pins.len());
            lint_board(&b)
        }
        (_, Ok(c)) => {
            println!("parsed as component '{}' ({} terminals)", c.id, c.terminals.len());
            lint_component(&c)
        }
        (Err(be), Err(ce)) => {
            eprintln!("does not parse as board: {be}");
            eprintln!("does not parse as component: {ce}");
            std::process::exit(1);
        }
    };
    if problems.is_empty() {
        println!("OK");
    } else {
        for p in &problems {
            eprintln!("LINT: {p}");
        }
        std::process::exit(1);
    }
}
