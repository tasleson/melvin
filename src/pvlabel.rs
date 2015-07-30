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

use std::io;
use std::io::{Read, Write, Result, Error, Seek, SeekFrom};
use std::io::ErrorKind::Other;
use std::path::{Path, PathBuf};
use std::fs::{File, read_dir, OpenOptions};
use std::cmp::min;
use std::slice::bytes::copy_memory;

use byteorder::{LittleEndian, ByteOrder};
use nix::sys::stat;

use parser::{LvmTextMap, textmap_to_buf, buf_to_textmap};
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

impl LabelHeader {
    fn from_buf(buf: &[u8]) -> Result<LabelHeader> {
        for x in 0..LABEL_SCAN_SECTORS {
            let sec_buf = &buf[x*SECTOR_SIZE..x*SECTOR_SIZE+SECTOR_SIZE];
            if &sec_buf[..8] == b"LABELONE" {
                let crc = LittleEndian::read_u32(&sec_buf[16..20]);
                if crc != crc32_calc(&sec_buf[20..SECTOR_SIZE]) {
                }

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

    fn write(&self, device: &Path) -> Result<()> {
        let mut sec_buf = [0u8; SECTOR_SIZE];

        copy_memory(self.id.as_bytes(), &mut sec_buf[..8]); // b"LABELONE"
        LittleEndian::write_u64(&mut sec_buf[8..16], self.sector);
        // switch back to "offset from label" from the more convenient "offset from start".
        LittleEndian::write_u32(
            &mut sec_buf[20..24], self.offset - (self.sector * SECTOR_SIZE as u64) as u32);
        copy_memory(self.label.as_bytes(), &mut sec_buf[24..32]);
        let crc_val = crc32_calc(&sec_buf[20..]);
        LittleEndian::write_u32(&mut sec_buf[16..20], crc_val);

        let mut f = try!(OpenOptions::new().write(true).open(device));
        try!(f.seek(SeekFrom::Start(self.sector * SECTOR_SIZE as u64)));
        f.write_all(&mut sec_buf)
    }
}

/// Describes an area within a PV
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct PvArea {
    /// The offset from the start of the device in bytes
    pub offset: u64,
    /// The size in bytes
    pub size: u64,
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

#[derive(Debug, PartialEq, Clone, Copy)]
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

/// A struct containing the values in the PV header. It contains pointers to
/// the data area, and possibly metadata areas and bootloader area.
#[derive(Debug)]
pub struct PvHeader {
    /// The unique identifier.
    pub uuid: String,
    /// Size in bytes of the entire PV
    pub size: u64,
    /// Extension version. If 1, we look for an extension header that may contain a reference
    /// to a bootloader area.
    pub ext_version: u32,
    /// Extension flags, of which there are none.
    pub ext_flags: u32,
    /// A list of the data areas.
    pub data_areas: Vec<PvArea>,
    /// A list of the metadata areas.
    pub metadata_areas: Vec<PvArea>,
    /// A list of the bootloader areas.
    pub bootloader_areas: Vec<PvArea>,
    /// The device this pvheader is for
    pub dev_path: PathBuf,
}

impl PvHeader {
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
    /// Parse a buf containing the on-disk pvheader and create a struct
    /// representing it.
    pub fn from_buf(buf: &[u8], path: &Path) -> Result<PvHeader> {

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
            dev_path: path.to_owned(),
        })
    }

    /// Find the PvHeader struct in a given device.
    pub fn find_in_dev(path: &Path) -> Result<PvHeader> {

        let mut f = try!(File::open(path));

        let mut buf = [0u8; LABEL_SCAN_SECTORS * SECTOR_SIZE];

        try!(f.read(&mut buf));

        let label_header = try!(LabelHeader::from_buf(&buf));
        let pvheader = try!(PvHeader::from_buf(&buf[label_header.offset as usize..], path));

        return Ok(pvheader);
    }

    fn get_rlocn0(buf: &[u8]) -> Option<RawLocn> {
        iter_raw_locn(&buf[40..]).next()
    }

    fn set_rlocn0(buf: &mut [u8], rl: &RawLocn) -> () {
        let mut raw_locn = &mut buf[40..];

        LittleEndian::write_u64(&mut raw_locn[..8], rl.offset);
        LittleEndian::write_u64(&mut raw_locn[8..16], rl.size);
        LittleEndian::write_u32(&mut raw_locn[16..20], rl.checksum);

        let flags = rl.ignored as u32;

        LittleEndian::write_u32(&mut raw_locn[20..24], flags);
    }

    /// Read the metadata contained in the metadata area.
    /// In the case of multiple metadata areas, return the information
    /// from the first valid one.
    pub fn read_metadata(&self) -> io::Result<LvmTextMap> {
        let mut f = try!(OpenOptions::new().read(true).open(&self.dev_path));

        for pvarea in &self.metadata_areas {
            let hdr = try!(Self::read_mda_header(&pvarea, &mut f));

            let rl = match Self::get_rlocn0(&hdr) {
                None => continue,
                Some(x) => x,
            };

            if rl.ignored {
                continue
            }

            let mut text = vec![0; rl.size as usize];
            let first_read = min(pvarea.size - rl.offset, rl.size) as usize;

            try!(f.seek(SeekFrom::Start(pvarea.offset + rl.offset)));
            try!(f.read(&mut text[..first_read]));

            if first_read != rl.size as usize {
                try!(f.seek(SeekFrom::Start(
                    pvarea.offset + MDA_HEADER_SIZE as u64)));
                try!(f.read(&mut text[rl.size as usize - first_read..]));
            }

            if rl.checksum != crc32_calc(&text) {
                return Err(Error::new(Other, "MDA text checksum failure"));
            }

            return buf_to_textmap(&text);
        }

        return Err(Error::new(Other, "No valid metadata found"));
    }

    /// Write the given metadata to all active metadata areas in the PV.
    pub fn write_metadata(&mut self, map: &LvmTextMap) -> io::Result<()> {

        let mut f = try!(OpenOptions::new().read(true).write(true)
                         .open(&self.dev_path));

        for pvarea in &self.metadata_areas {
            let mut hdr = try!(Self::read_mda_header(&pvarea, &mut f));

            // If this is the first write, supply an initial RawLocn template
            let rl = match Self::get_rlocn0(&hdr) {
                None => RawLocn {
                    offset: MDA_HEADER_SIZE as u64,
                    size: 0,
                    checksum: 0,
                    ignored: false,
                },
                Some(x) => x,
            };

            if rl.ignored {
                continue
            }

            let mut text = textmap_to_buf(map);
            // Ends with one null
            text.push(b'\0');

            // start at next sector in loop, but skip 0th sector
            let start_off = min(MDA_HEADER_SIZE as u64,
                                (align_to(
                                    (rl.offset + rl.size) as usize,
                                    SECTOR_SIZE)
                                 % pvarea.size as usize) as u64);
            let tail_space = pvarea.size as u64 - start_off;

            assert_eq!(start_off % SECTOR_SIZE as u64, 0);
            assert_eq!(tail_space % SECTOR_SIZE as u64, 0);

            let written = if tail_space != 0 {
                try!(f.seek(
                    SeekFrom::Start(pvarea.offset + start_off)));
                try!(f.write_all(&text[..min(tail_space as usize, text.len())]));
                min(tail_space as usize, text.len())
            } else {
                0
            };

            if written != text.len() {
                try!(f.seek(
                    SeekFrom::Start(pvarea.offset + MDA_HEADER_SIZE as u64)));
                try!(f.write_all(&text[written as usize..]));
            }

            Self::set_rlocn0(&mut hdr,
                &RawLocn {
                    offset: start_off,
                    size: text.len() as u64,
                    checksum: crc32_calc(&text),
                    ignored: rl.ignored,
                });

            try!(Self::write_mda_header(&pvarea, &mut hdr, &mut f));
        }

        Ok(())
    }

    fn read_mda_header(area: &PvArea, file: &mut File)
                        -> io::Result<[u8; MDA_HEADER_SIZE]> {
        assert!(area.size as usize > MDA_HEADER_SIZE);
        try!(file.seek(SeekFrom::Start(area.offset)));
        let mut hdr = [0u8; MDA_HEADER_SIZE];
        try!(file.read(&mut hdr));

        if LittleEndian::read_u32(&hdr[..4]) != crc32_calc(&hdr[4..MDA_HEADER_SIZE]) {
            return Err(Error::new(Other, "MDA header checksum failure"));
        }

        if &hdr[4..20] != MDA_MAGIC {
            return Err(Error::new(
                Other, format!("'{}' doesn't match MDA_MAGIC",
                               String::from_utf8_lossy(&hdr[4..20]))));
        }

        let ver = LittleEndian::read_u32(&hdr[20..24]);
        if ver != 1 {
            return Err(Error::new(Other, "Bad version, expected 1"));
        }

        // TODO: validate these somehow
        //println!("mdah start {}", LittleEndian::read_u64(&buf[24..32]));
        //println!("mdah size {}", LittleEndian::read_u64(&buf[32..40]));
        Ok(hdr)
    }


    fn write_mda_header(area: &PvArea, hdr: &mut [u8; MDA_HEADER_SIZE], file: &mut File)
                        -> io::Result<()> {
        let csum = crc32_calc(&hdr[4..]);
        LittleEndian::write_u32(&mut hdr[..4], csum);

        try!(file.seek(SeekFrom::Start(area.offset)));
        try!(file.write_all(hdr));

        Ok(())
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
            .filter(|path| { PvHeader::find_in_dev(&path).is_ok() })
            .collect::<Vec<_>>());
    }

    Ok(ret_vec)
}
