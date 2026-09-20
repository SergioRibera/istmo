//! Extract `#[istmo::message]` type docs from one or more plugin
//! `src/lib.rs` files and write matching MDX partials.
//!
//! Usage:
//!
//! ```text
//! docs-types --out <docs-generated-dir> \
//!     <slug>=<path-to-lib.rs> [<slug>=<path-to-lib.rs> ...]
//! ```
//!
//! Each `<slug>` becomes `<out>/<slug>-types.mdx`. `--out` is required.
//!
//! Exits non-zero on parse / IO failure; safe to invoke from a
//! `justfile` recipe as part of the docs pipeline.

use std::path::PathBuf;
use std::process::ExitCode;

use istmo_build::{extract_message_docs, render_types_mdx, write_types_mdx};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut out_dir: Option<PathBuf> = None;
    let mut jobs: Vec<(String, PathBuf)> = Vec::new();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" | "-o" => {
                let Some(next) = args.next() else {
                    eprintln!("docs-types: --out expects a directory path");
                    return ExitCode::from(2);
                };
                out_dir = Some(PathBuf::from(next));
            }
            "-h" | "--help" => {
                print_usage();
                return ExitCode::SUCCESS;
            }
            other => {
                let Some((slug, path)) = other.split_once('=') else {
                    eprintln!(
                        "docs-types: expected `<slug>=<path>`, got `{other}` (use --help for usage)"
                    );
                    return ExitCode::from(2);
                };
                jobs.push((slug.to_owned(), PathBuf::from(path)));
            }
        }
    }

    let Some(out_dir) = out_dir else {
        eprintln!("docs-types: --out <dir> is required");
        return ExitCode::from(2);
    };
    if jobs.is_empty() {
        eprintln!("docs-types: at least one <slug>=<path> pair is required");
        return ExitCode::from(2);
    }

    let mut failed = false;
    for (slug, source) in jobs {
        let dest = out_dir.join(format!("{slug}-types.mdx"));
        match extract_message_docs(&source) {
            Ok(types) => {
                let mdx = render_types_mdx(&types);
                if let Err(err) = write_types_mdx(&dest, &mdx) {
                    eprintln!(
                        "docs-types: failed to write {}: {err}",
                        dest.display()
                    );
                    failed = true;
                } else {
                    println!("docs-types: wrote {} ({} types)", dest.display(), types.len());
                }
            }
            Err(err) => {
                eprintln!(
                    "docs-types: failed to extract {}: {err}",
                    source.display()
                );
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn print_usage() {
    println!(
        "Usage: docs-types --out <dir> <slug>=<path/to/src/lib.rs> [...]\n\n\
         Example:\n\
             docs-types --out docs/src/generated/plugins \\\n\
                 data-store=plugins/data-store/src/lib.rs \\\n\
                 google-sign-in=plugins/google-sign-in/src/lib.rs\n"
    );
}
