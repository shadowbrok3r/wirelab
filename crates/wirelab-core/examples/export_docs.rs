//! Writes the schematic, perfboard drawing and net/wire tables for a project.
//!
//! `cargo run -p wirelab-core --example export_docs -- <project.wirelab.json> <out_dir>`

use std::path::PathBuf;

use wirelab_core::library::Library;
use wirelab_core::project::Project;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(project), Some(out)) = (args.next(), args.next()) else {
        eprintln!("usage: export_docs <project.wirelab.json> <out_dir>");
        std::process::exit(2);
    };
    let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");
    let lib = Library::load(&assets.join("boards"), &assets.join("components")).expect("assets load");
    let project = Project::load(std::path::Path::new(&project)).expect("project loads");
    let multi = project.boards.len() > 1;
    for tab in &project.boards {
        let board = lib.board(&tab.circuit.board_id).expect("known board");
        let name = if multi { format!("{} {}", project.name, tab.name) } else { project.name.clone() };
        let dir = PathBuf::from(&out);
        match wirelab_core::export::export_all(&name, &tab.circuit, board, &lib, &dir) {
            Ok(files) => {
                for f in files {
                    println!("{}", f.display());
                }
            }
            Err(e) => {
                eprintln!("export failed: {e}");
                std::process::exit(1);
            }
        }
    }
}
