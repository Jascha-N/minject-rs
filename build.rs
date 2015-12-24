use std::{io, env};
use std::process::Command;
use std::io::prelude::*;

fn main() {
    let target = env::var("TARGET").unwrap();

    let parts = target.splitn(4, '-').collect::<Vec<_>>();
    let arch = parts[0];
    let sys = parts[2];

    if sys != "windows" {
        panic!("Platform '{}' not supported.", sys);
    }

    let input = match arch {
        "i686"   => "src/stub32.asm",
        "x86_64" => "src/stub64.asm",
        _        => panic!("Architecture '{}' not supported.", arch)
    };

    let status = Command::new("fasm")
                         .arg(input)
                         .status()
                         .unwrap();
                         
    if !status.success() {
        panic!("'fasm' exited with code: {}.", status.code().unwrap())
    }
}