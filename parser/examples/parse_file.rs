use std::{env, process::ExitCode};

use xml_parser::XmlDom;

fn main() -> ExitCode {
    let mut paths = env::args().skip(1).peekable();
    if matches!(paths.peek().map(String::as_str), Some("-h" | "--help")) {
        println!("usage: parse_file XML_FILE...");
        return ExitCode::SUCCESS;
    }
    if paths.peek().is_none() {
        eprintln!("usage: parse_file XML_FILE...");
        return ExitCode::FAILURE;
    }

    let mut ok = true;

    for path in paths {
        match parse_path(&path) {
            Ok(summary) => {
                println!(
                    "{}\troot={}\telements={}\tattributes={}\tnodes={}",
                    path, summary.root, summary.elements, summary.attributes, summary.nodes
                );
            }
            Err(error) => {
                eprintln!("{path}: {error}");
                ok = false;
            }
        }
    }

    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn parse_path(path: &str) -> Result<Summary, String> {
    let document = XmlDom::load(path).map_err(|error| error.to_string())?;
    let stats = document.tree_stats();
    Ok(Summary {
        root: document
            .root()
            .name()
            .map_err(|error| error.to_string())?
            .unwrap_or_default(),
        elements: stats.elements,
        attributes: stats.attributes,
        nodes: stats.nodes,
    })
}

struct Summary {
    root: String,
    elements: usize,
    attributes: usize,
    nodes: usize,
}
