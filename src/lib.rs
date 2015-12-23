extern crate winapi as w;
extern crate kernel32 as k32;
extern crate byteorder;
extern crate miow;

mod handle;
mod inject;

pub mod process;
pub use inject::Module;