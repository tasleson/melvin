// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//#![warn(missing_docs)]

//! Melvin is a library for configuring logical volumes in the style of
//! [LVM](https://www.sourceware.org/lvm2/)

extern crate byteorder;
extern crate crc;
extern crate libc;
extern crate nix;
extern crate time;
extern crate unix_socket;
extern crate uuid;

mod error;
mod lv;
pub mod parser;
mod pv;
mod pvlabel;
mod util;
mod vg;

pub use error::{Error, Result};
pub use lv::LV;
pub use pv::PV;
pub use pvlabel::{pvheader_scan, PvHeader};
pub use vg::VG;
