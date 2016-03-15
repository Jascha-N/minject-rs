use std::env;
use std::path::Path;
use std::process::Command;

fn main() {
    let target = env::var("TARGET").unwrap();
    let out_dir = env::var("OUT_DIR").unwrap();

    let parts = target.splitn(4, '-').collect::<Vec<_>>();
    let arch = parts[0];
    let sys = parts[2];

    if sys != "windows" {
        panic!("Platform '{}' not supported.", sys);
    }

    let input = match arch {
        "i686"   => "src/thunk32.asm",
        "x86_64" => "src/thunk64.asm",
        _        => panic!("Architecture '{}' not supported.", arch)
    };

    let status = Command::new("fasm")
                         .arg(input)
                         .arg(Path::new(&out_dir).join("thunk.bin"))
                         .status()
                         .unwrap();

    if !status.success() {
        panic!("'fasm' exited with code: {}.", status.code().unwrap())
    }

    println!("cargo:rerun-if-changed={}", input);
}