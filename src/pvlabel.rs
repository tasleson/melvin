// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//! Reading and writing LVM on-disk labels and metadata.

//
// label is at start of sectors 0-3, usually 1
// label includes offset of pvheader (also within 1st 4 sectors)
// pvheader includes ptrs to data (1), metadata(0-2), and boot(0-1) areas
// metadata area (MDA), located anywhere, starts with 512b mda header, then
//   large text area
// mda header has 40b of stuff, then rlocns[].
// rlocns point into mda text area. rlocn 0 used for text metadata, rlocn 1
//   points to precommitted data (not currently supported by Melvin)
// text metadata written aligned to sector-size; text area treated as circular
//   and text may wrap across end to beginning
// text metadata contains vg metadata in lvm config text format. Each write
//   increments seqno.
//

use std::io::{Read, Write, Result, Error, Seek, SeekFrom};
use std::io::ErrorKind::Other;
use std::path::{Path, PathBuf};
use std::fs::{File, read_dir, OpenOptions};
use std::cmp::min;
use std::slice::bytes::copy_memory;

use byteorder::{LittleEndian, ByteOrder};
use nix::sys::stat;

use parser;

use parser::{LvmTextMap, buf_to_textmap, textmap_to_buf};
use util::{align_to, crc32_calc};

const LABEL_SCAN_SECTORS: usize = 4;
const ID_LEN: usize = 32;
const MDA_MAGIC: &'static [u8] = b"\x20\x4c\x56\x4d\x32\x20\x78\x5b\x35\x41\x25\x72\x30\x4e\x2a\x3e";
const SECTOR_SIZE: usize = 512;
const MDA_HEADER_SIZE: usize = 512;

#[derive(Debug)]
struct LabelHeader {
    id: String,
    sector: u64,
    crc: u32,
    offset: u32,
    label: String,
}

#[derive(Debug)]
struct PvArea {
    offset: u64,
    size: u64,
}

#[derive(Debug)]
struct PvHeader {
    uuid: String,
    size: u64, // in bytes
    ext_version: u32,
    ext_flags: u32,
    data_areas: Vec<PvArea>,
    metadata_areas: Vec<PvArea>,
    bootloader_areas: Vec<PvArea>,
}


fn label_header_from_buf(buf: &[u8]) -> Result<LabelHeader> {

    for x in 0..LABEL_SCAN_SECTORS {
        let sec_buf = &buf[x*SECTOR_SIZE..x*SECTOR_SIZE+SECTOR_SIZE];
        if &sec_buf[..8] == b"LABELONE" {
            let crc = LittleEndian::read_u32(&sec_buf[16..20]);
            crc32_ok(crc, &sec_buf[20..SECTOR_SIZE]);

            let sector = LittleEndian::read_u64(&sec_buf[8..16]);
            if sector != x as u64 {
                return Err(Error::new(Other, "Sector field should equal sector count"));
            }

            return Ok(LabelHeader{
                id: String::from_utf8_lossy(&sec_buf[..8]).into_owned(),
                sector: sector,
                crc: crc,
                // switch from "offset from label" to "offset from start", more convenient.
                offset: LittleEndian::read_u32(&sec_buf[20..24]) + (x*SECTOR_SIZE as usize) as u32,
                label: String::from_utf8_lossy(&sec_buf[24..32]).into_owned(),
            })
        }
    }

    Err(Error::new(Other, "Label not found"))
}

fn write_label_header(label: &LabelHeader, device: &Path) -> Result<()> {
    let mut sec_buf = [0u8; SECTOR_SIZE];

    copy_memory(label.id.as_bytes(), &mut sec_buf[..8]); // b"LABELONE"
    LittleEndian::write_u64(&mut sec_buf[8..16], label.sector);
    // switch back to "offset from label" from the more convenient "offset from start".
    LittleEndian::write_u32(
        &mut sec_buf[20..24], label.offset - (label.sector * SECTOR_SIZE as u64) as u32);
    copy_memory(label.label.as_bytes(), &mut sec_buf[24..32]);
    let crc_val = crc32_calc(&sec_buf[20..]);
    LittleEndian::write_u32(&mut sec_buf[16..20], crc_val);

    let mut f = try!(OpenOptions::new().write(true).open(device));
    try!(f.seek(SeekFrom::Start(label.sector * SECTOR_SIZE as u64)));
    f.write_all(&mut sec_buf)
}

#[derive(Debug)]
struct PvAreaIter<'a> {
    area: &'a[u8],
}

fn iter_pv_area<'a>(buf: &'a[u8]) -> PvAreaIter<'a> {
    PvAreaIter { area: buf }
}

impl<'a> Iterator for PvAreaIter<'a> {
    type Item = PvArea;

    fn next (&mut self) -> Option<PvArea> {
        let off = LittleEndian::read_u64(&self.area[..8]);
        let size = LittleEndian::read_u64(&self.area[8..16]);

        if off == 0 {
            None
        }
        else {
            self.area = &self.area[16..];
            Some(PvArea {
                offset: off,
                size: size,
            })
        }
    }
}

//
// PV HEADER LAYOUT:
// - static header (uuid and size)
// - 0+ data areas (actually max 1, usually 1; size 0 == "rest of blkdev")
// - blank entry
// - 0+ metadata areas (max 2, usually 1)
// - blank entry
// - 8 bytes of pvextension header
// - if version > 0
//   - 0+ bootloader areas (usually 0)
//
fn pv_header_from_buf(buf: &[u8]) -> Result<PvHeader> {

    let mut da_buf = &buf[ID_LEN+8..];

    let da_vec: Vec<_> = iter_pv_area(da_buf).collect();

    // move slice past any actual entries plus blank
    // terminating entry
    da_buf = &da_buf[(da_vec.len()+1)*16..];

    let md_vec: Vec<_> = iter_pv_area(da_buf).collect();

    da_buf = &da_buf[(md_vec.len()+1)*16..];

    let ext_version = LittleEndian::read_u32(&da_buf[..4]);
    let mut ext_flags = 0;
    let mut ba_vec = Vec::new();

    if ext_version != 0 {
        ext_flags = LittleEndian::read_u32(&da_buf[4..8]);

        da_buf = &da_buf[8..];

        ba_vec = iter_pv_area(da_buf).collect();
    }

    Ok(PvHeader{
        uuid: String::from_utf8_lossy(&buf[..ID_LEN]).into_owned(),
        size: LittleEndian::read_u64(&buf[ID_LEN..ID_LEN+8]),
        ext_version: ext_version,
        ext_flags: ext_flags,
        data_areas: da_vec,
        metadata_areas: md_vec,
        bootloader_areas: ba_vec,
    })
}

#[derive(Debug, PartialEq, Clone)]
struct RawLocn {
    offset: u64,
    size: u64,
    checksum: u32,
    ignored: bool,
}

#[derive(Debug)]
struct RawLocnIter<'a> {
    area: &'a[u8],
}

fn iter_raw_locn<'a>(buf: &'a[u8]) -> RawLocnIter<'a> {
    RawLocnIter { area: buf }
}

impl<'a> Iterator for RawLocnIter<'a> {
    type Item = RawLocn;

    fn next (&mut self) -> Option<RawLocn> {
        let off = LittleEndian::read_u64(&self.area[..8]);
        let size = LittleEndian::read_u64(&self.area[8..16]);
        let checksum = LittleEndian::read_u32(&self.area[16..20]);
        let flags = LittleEndian::read_u32(&self.area[20..24]);

        if off == 0 {
            None
        }
        else {
            self.area = &self.area[24..];
            Some(RawLocn {
                offset: off,
                size: size,
                checksum: checksum,
                ignored: (flags & 1) > 0,
            })
        }
    }
}

fn find_pvheader_in_dev(path: &Path) -> Result<PvHeader> {

    let mut f = try!(File::open(path));

    let mut buf = [0u8; LABEL_SCAN_SECTORS * SECTOR_SIZE];

    try!(f.read(&mut buf));

    let label_header = try!(label_header_from_buf(&buf));
    let pvheader = try!(pv_header_from_buf(&buf[label_header.offset as usize..]));

    return Ok(pvheader);
}

/// A handle to an LVM on-disk metadata area (MDA)
pub struct MDA {
    file: File,
    hdr: [u8; MDA_HEADER_SIZE],
    area_offset: u64,
    area_len: u64,
}

impl MDA {
    /// Construct an MDA given a path to a block device containing an LVM Physical Volume (PV)
    pub fn new(path: &Path) -> Result<MDA> {
        let pvheader = try!(find_pvheader_in_dev(&path));

        let mut f = try!(OpenOptions::new().read(true).write(true).open(path));

        let mda_areas = pvheader.metadata_areas.len();
        if mda_areas != 1 {
            return Err(Error::new(
                Other, format!("Expecting 1 mda, found {}", mda_areas)));
        }

        let md = &pvheader.metadata_areas[0];
        assert!(md.size as usize > MDA_HEADER_SIZE);
        try!(f.seek(SeekFrom::Start(md.offset)));
        let mut buf = [0; MDA_HEADER_SIZE];
        try!(f.read(&mut buf));

        if !crc32_ok(LittleEndian::read_u32(&buf[..4]), &buf[4..MDA_HEADER_SIZE]) {
            return Err(Error::new(Other, "MDA header checksum failure"));
        }

        if &buf[4..20] != MDA_MAGIC {
            return Err(Error::new(
                Other, format!("'{}' doesn't match MDA_MAGIC",
                               String::from_utf8_lossy(&buf[4..20]))));
        }

        let ver = LittleEndian::read_u32(&buf[20..24]);
        if ver != 1 {
            return Err(Error::new(Other, "Bad version, expected 1"));
        }

        // TODO: validate these somehow
        //println!("mdah start {}", LittleEndian::read_u64(&buf[24..32]));
        //println!("mdah size {}", LittleEndian::read_u64(&buf[32..40]));

        Ok(MDA {
            file: f,
            hdr: buf,
            area_offset: md.offset,
            area_len: md.size,
        })
    }

    fn write_mda_header(&mut self) -> Result<()> {
        let csum = crc32_calc(&self.hdr[4..]);
        LittleEndian::write_u32(&mut self.hdr[..4], csum);

        try!(self.file.seek(SeekFrom::Start(self.area_offset)));
        try!(self.file.write_all(&mut self.hdr));

        Ok(())
    }

    fn get_rlocn0(&self) -> RawLocn {
        let raw_locns: Vec<_> = iter_raw_locn(&self.hdr[40..]).collect();
        let rlocn_len = raw_locns.len();
        if rlocn_len != 1 {
            panic!("Expecting 1 rlocn, found {}", rlocn_len);
        }

        raw_locns[0].clone()
    }

    fn set_rlocn0(&mut self, rl: &RawLocn) -> () {
        let mut raw_locn = &mut self.hdr[40..];

        LittleEndian::write_u64(&mut raw_locn[..8], rl.offset);
        LittleEndian::write_u64(&mut raw_locn[8..16], rl.size);
        LittleEndian::write_u32(&mut raw_locn[16..20], rl.checksum);

        let flags = rl.ignored as u32;

        LittleEndian::write_u32(&mut raw_locn[20..24], flags);
    }

    /// Read the metadata contained in the metadata area.
    pub fn read_metadata(&mut self) -> Result<LvmTextMap> {
        let rl = self.get_rlocn0();

        let mut text = vec![0; rl.size as usize];
        let first_read = min(self.area_len - rl.offset, rl.size) as usize;

        try!(self.file.seek(SeekFrom::Start(self.area_offset + rl.offset)));
        try!(self.file.read(&mut text[..first_read]));

        if first_read != rl.size as usize {
            try!(self.file.seek(SeekFrom::Start(
                self.area_offset + MDA_HEADER_SIZE as u64)));
            try!(self.file.read(&mut text[rl.size as usize - first_read..]));
        }

        if !crc32_ok(rl.checksum, &text) {
            return Err(Error::new(Other, "MDA text checksum failure"));
        }

        parser::buf_to_textmap(&text)
    }

    /// Write a new version of the metadata to the metadata area.
    pub fn write_metadata(&mut self, map: &LvmTextMap) -> Result<()> {
        let raw_locn = self.get_rlocn0();

        let mut text = textmap_to_buf(map);
        // Ends with one null
        text.push(b'\0');

        // start at next sector in loop, but skip 0th sector
        let start_off = min(MDA_HEADER_SIZE as u64,
                            (align_to(
                                (raw_locn.offset + raw_locn.size) as usize,
                                SECTOR_SIZE)
                             % self.area_len as usize) as u64);
        let tail_space = self.area_len as u64 - start_off;

        assert_eq!(start_off % SECTOR_SIZE as u64, 0);
        assert_eq!(tail_space % SECTOR_SIZE as u64, 0);

        let written = if tail_space != 0 {
            try!(self.file.seek(
                SeekFrom::Start(self.area_offset + start_off)));
            try!(self.file.write_all(
                &text[..min(tail_space as usize, text.len())]));
            min(tail_space as usize, text.len())
        } else {
            0
        };

        if written != text.len() {
            try!(self.file.seek(
                SeekFrom::Start(self.area_offset as u64 + MDA_HEADER_SIZE as u64)));
            try!(self.file.write_all(
                &text[written as usize..]));
        }

        self.set_rlocn0(
            &RawLocn {
                offset: start_off,
                size: text.len() as u64,
                checksum: crc32_calc(&text),
                ignored: false,
            });

        self.write_mda_header()
    }
}

/// Scan a list of directories for block devices containing LVM PV labels.
pub fn scan_for_pvs(dirs: &[&Path]) -> Result<Vec<PathBuf>> {

    let mut ret_vec = Vec::new();

    for dir in dirs {
        ret_vec.extend(try!(read_dir(dir))
            .into_iter()
            .filter_map(|dir_e| if dir_e.is_ok()
                        { Some(dir_e.unwrap().path()) } else {None} )
            .filter(|path| {
                (stat::stat(path).unwrap().st_mode & 0x6000) == 0x6000 }) // S_IFBLK
            .filter(|path| { find_pvheader_in_dev(&path).is_ok() })
            .collect::<Vec<_>>());
    }

    Ok(ret_vec)
}
