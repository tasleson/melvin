// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

#![feature(result_expect)]

extern crate melvin;

use std::path;
use std::io::Result;
use std::io::Error;
use std::io::ErrorKind::Other;
use std::path::Path;

use melvin::{VG, PvHeader, pvheader_scan};
use melvin::parser;

fn print_pvheaders() -> Result<()> {
    let dirs = vec![path::Path::new("/dev")];

    for pvheader in try!(pvheader_scan(&dirs)) {
        println!("pvheader {:#?}", pvheader);
    }

    Ok(())
}


fn get_first_vg_meta() -> Result<(String, parser::LvmTextMap)> {
    let dirs = vec![path::Path::new("/dev")];

    for pv_path in try!(pvheader_scan(&dirs)) {
        let pvheader = try!(PvHeader::find_in_dev(&pv_path));
        let map = try!(pvheader.read_metadata());

        // Find the textmap for the vg, among all the other stuff.
        // (It's the only textmap.)
        for (key, value) in map {
            match value {
                parser::Entry::TextMap(x) => return Ok((key, *x)),
                _ => {}
            }
        }
    }

    Err(Error::new(Other, "dude"))
}

fn get_conf() -> Result<parser::LvmTextMap> {
    use std::fs;
    use std::io::Read;

    let mut f = try!(fs::File::open("/etc/lvm/lvm.conf"));

    let mut buf = Vec::new();
    try!(f.read_to_end(&mut buf));

    parser::buf_to_textmap(&buf)
}

fn main() {
    // println!("{:?}", PvHeader::initialize(Path::new("/dev/vdc1")));
    // print_pvheaders();
    // let (name, map) = get_first_vg_meta().unwrap();
    // let vg = parser::vg_from_textmap(&name, &map).expect("didn't get vg!");
    // let mut vgs = lvmetad::vg_list().expect("could not get vgs from lvmetad");
    // let mut vg = vgs.pop().expect("no vgs in vgs");

    let path1 = Path::new("/dev/vdc1");
    let path2 = Path::new("/dev/vdc2");

//    let pvh1 = PvHeader::find_in_dev(Path::new("/dev/vdc1")).expect("pvheader not found");

    let mut vg = VG::create("vg-dopey", vec![path1, path2]).expect("vgcreate failed yo");
    // vg.add_pv(&pvh1).unwrap();
    // vg.add_pv(&pvh2).unwrap();




    // let mut vgs = lvmetad::vgs_from_lvmetad().expect("could not get vgs from lvmetad");
    // let mut vg = vgs.pop().expect("no vgs in vgs");

    // match vg.new_linear_lv("grover125", 2021) {
    //     Ok(_) => {},
    //     Err(x) => {
    //         println!("err {:?}", x);
    //         return;
    //     }
    // };

    // match vg.lv_remove("grover125") {
    //     Ok(_) => {println!("yay")},
    //     Err(x) => {
    //         println!("err {:?}", x);
    //         return;
    //     }
    // };

    // for (lvname, lv) in &vg.lvs {
    //     println!("lv2 {:?}", lv);
    // }

    // let tm = get_conf().expect("could not read lvm.conf");
    // let locking_type = tm.textmap_from_textmap("global")
    //     .and_then(|g| g.i64_from_textmap("locking_type")).unwrap();

    // println!("locking_type = {}", locking_type);

    // let vgtm = vg.into();
    // let s = parser::textmap_to_buf(&vgtm);
}
