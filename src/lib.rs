//! # The minject-rs library
//! This library provides a simple API for injecting code (in the form of a DLL)
//! into another process on the Windows platform.
#![warn(missing_docs)]

extern crate winapi as w;
extern crate kernel32 as k32;
extern crate byteorder;
extern crate miow;
extern crate serde;
extern crate bincode;

mod handle;
mod inject;

#[macro_use]
pub mod init;
pub mod process;
pub use inject::{Module, ModuleBuilder, ModuleBuilderWithInit};