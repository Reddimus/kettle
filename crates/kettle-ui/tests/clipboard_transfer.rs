pub use arboard::{Clear, Get, Set};

#[allow(dead_code, unexpected_cfgs)]
#[rustfmt::skip]
#[path = "../../../vendor/arboard-3.6.1/src/common.rs"]
mod common;
pub use common::Error;

#[rustfmt::skip]
#[path = "../../../vendor/arboard-3.6.1/src/platform/x11_incr.rs"]
mod x11_incr;
