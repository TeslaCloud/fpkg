//! Evaluates one packagespec.lua in this process and writes the manifest as
//! JSON to stdout: the child half of `fpkg_core::spec`'s process evaluator.

fn main() {
    let request = std::env::args().nth(1);
    std::process::exit(fpkg_core::spec::child_main(request.as_deref()));
}
