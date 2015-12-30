extern crate winapi as w;
extern crate kernel32 as k32;
extern crate byteorder;
extern crate miow;
extern crate serde;
extern crate serde_json;

mod handle;
mod inject;

#[macro_use]
pub mod init;
pub mod process;
pub use inject::{Module, ModuleBuilder, ModuleBuilderWithInit};