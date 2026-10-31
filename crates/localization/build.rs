use std::{env, fs, path::PathBuf};

fn main() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR not set"));
    fs::write(out_dir.join("translator_credits.txt"), "").expect("failed to write translator credits");
}
