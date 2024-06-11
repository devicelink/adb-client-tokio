#![crate_type = "lib"]
#![forbid(unsafe_code)]
#![forbid(missing_debug_implementations)]
#![forbid(missing_docs)]
#![doc = include_str!("../README.md")]

mod client;
mod util;
mod shell;
mod connection;

pub use util::*;
pub use client::*;
