//! The sandbox launcher on its own, for the tests that shrink pictures and
//! open files with Docs. On a server, the server's own program is the
//! launcher, and it reads files for Open with Docs too.

use accordion_docs::import;

fn main() {
    let mut args = std::env::args_os().skip(1);
    match args.next() {
        Some(command) if command == import::COMMAND => std::process::exit(import::main(args)),
        _ => accordion_sandbox::main(),
    }
}
