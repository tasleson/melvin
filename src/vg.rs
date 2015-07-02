use std::io::Result;
use std::io::Error;
use std::io::ErrorKind::Other;
use std::collections::btree_map::BTreeMap;

use uuid::Uuid;
use time::now;
use nix;

use lv::{LV, Segment};
use pv::PV;

#[derive(Debug, PartialEq, Clone)]
pub struct VG {
    pub name: String,
    pub id: String,
    pub seqno: u64,
    pub format: String,
    pub status: Vec<String>,
    pub flags: Vec<String>,
    pub extent_size: u64,
    pub max_lv: u64,
    pub max_pv: u64,
    pub metadata_copies: u64,
    pub pvs: BTreeMap<String, PV>,
    pub lvs: BTreeMap<String, LV>,
}

impl VG {
    pub fn extents_in_use(&self) -> u64 {
        self.lvs
            .values()
            .map(|x| x.used_extents())
            .sum()
    }

    pub fn extents_free(&self) -> u64 {
        self.extents() - self.extents_in_use()
    }

    pub fn extents(&self) -> u64 {
        self.pvs
            .values()
            .map(|x| x.pe_count)
            .sum()
    }

    pub fn new_linear_lv(&mut self, name: &str, extent_size: u64) -> Result<()> {
        if self.lvs.contains_key(name) {
            return Err(Error::new(Other, "LV already exists"));
        }

        let mut contig_area = None;
        for (pvname, areas) in self.free_areas() {
            for (start, len) in areas {
                if len >= extent_size {
                    contig_area = Some((pvname, start));
                    break;
                }
            }
        }

        // we don't support multiple segments yet
        let (pv_with_area, area_start) = match contig_area {
            None => return Err(Error::new(Other, "no contiguous area for new LV")),
            Some(x) => x,
        };

        let segment = Segment {
            name: "segment1".to_string(),
            start_extent: area_start,
            extent_count: extent_size,
            ty: "striped".to_string(),
            stripes: vec![(pv_with_area, area_start)],
        };

        let lv = LV {
            name: name.to_string(),
            id: Uuid::new_v4().to_hyphenated_string(),
            status: vec!["READ".to_string(), "WRITE".to_string(), "VISIBLE".to_string()],
            flags: Vec::new(),
            creation_host: nix::sys::utsname::uname().nodename().to_string(),
            creation_time: now().to_timespec().sec,
            segments: vec![segment],
        };

        self.lvs.insert(name.to_string(), lv);

        // write metadata to metadata areas
        // commit metadata
        // tell lvmetad
        // poke dm and tell it about a new device
        // open champagne

        Ok(())
    }

    pub fn used_areas(&self) -> BTreeMap<String, BTreeMap<u64, u64>> {
        let mut used_map = BTreeMap::new();

        // pretty sure this is only correct for my system...
        for (lvname, lv) in &self.lvs {
            for seg in &lv.segments {
                for &(ref pvname, start) in &seg.stripes {
                    used_map.entry(pvname.to_string()).or_insert(BTreeMap::new())
                        .insert(start as u64, seg.extent_count);
                }
            }
        }

        used_map
    }

    pub fn free_areas(&self) -> BTreeMap<String, BTreeMap<u64, u64>> {
        let mut free_map = BTreeMap::new();

        for (pvname, area_map) in &mut self.used_areas() {

            // Insert an entry to mark the end of the PV so the fold works correctly
            let pv = self.pvs.get(pvname).expect("area map name refers to nonexistent PV");
            area_map.insert(pv.pe_count, 0);

            area_map.iter()
                .fold(0, |prev_end, (start, len)| {
                    if prev_end < *start {
                        free_map.entry(pvname.clone()).or_insert(BTreeMap::new())
                            .insert(prev_end, start-prev_end);
                    }
                    start + len
                });
        }

        free_map
    }
}