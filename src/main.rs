#![feature(iter_arith, result_expect)]

extern crate byteorder;
extern crate crc;
extern crate unix_socket;
extern crate nix;
extern crate libc;
extern crate regex;

use std::path;
use std::io::Result;
use std::io::Error;
use std::io::ErrorKind::Other;

mod parser;
mod lvmetad;
mod pvlabel;
mod dm;
mod lv;
mod vg;
mod pv;

#[allow(dead_code, non_camel_case_types)]
mod dm_ioctl;

use parser::LvmTextMap;
use parser::TextMapOps;

fn get_first_vg_meta() -> Result<(String, LvmTextMap)> {
    let dirs = vec![path::Path::new("/dev")];

    for pv in try!(pvlabel::scan_for_pvs(&dirs)) {
        let map = try!(pvlabel::textmap_from_dev(pv.as_path()));

        for (key, value) in map {
            match value {
                parser::Entry::TextMap(x) => return Ok((key, *x)),
                _ => {}
            }
        }
    }

    Err(Error::new(Other, "dude"))
}

fn main() {
    // println!("A");
    // let (name, map) = get_first_vg_meta().unwrap();
    // println!("B {}", name);
    // let vg = parser::vg_from_textmap(&name, &map).expect("didn't get vg!");
    // println!("heyo {} {}", vg.extents(), vg.extent_size);
    // println!("output {:?}", vg);

    // match dm::list_devices() {
    //     Ok(x) => println!("{:?}", x),
    //     Err(x) => println!("error {}", x),
    // }

    let vgs = lvmetad::vgs_from_lvmetad().expect("could not get vgs from lvmetad");
    for vg in &vgs {
        println!("{} tot {} alloc {} free {}",
                 vg.name, vg.extents(), vg.extents_in_use(), vg.extents_free());
        for lv in vg.lvs.keys() {
            println!("lv {}", lv);
        }
    }

    let tm = pvlabel::get_conf().expect("could not read lvm.conf");
    let locking_type = tm.textmap_from_textmap("global")
        .and_then(|g| g.i64_from_textmap("locking_type")).unwrap();

    println!("locking_type = {}", locking_type);
}
