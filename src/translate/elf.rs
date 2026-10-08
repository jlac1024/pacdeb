// SPDX-License-Identifier: AGPL-3.0-or-later
//! Just enough ELF reading to list a binary's DT_NEEDED libraries.

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const DT_NULL: u64 = 0;
const DT_NEEDED: u64 = 1;
const DT_STRTAB: u64 = 5;

pub const MAGIC: &[u8; 4] = b"\x7fELF";

/// The libraries a 64 bit little endian ELF file asks for. None when the file is not
/// one pacdeb can read (32 bit, big endian, truncated), or is statically linked.
pub fn needed(data: &[u8]) -> Option<Vec<String>> {
    if data.get(..4)? != MAGIC || *data.get(4)? != 2 || *data.get(5)? != 1 {
        return None;
    }
    let phoff = u64_at(data, 0x20)? as usize;
    let phentsize = u16_at(data, 0x36)? as usize;
    let phnum = u16_at(data, 0x38)? as usize;

    let mut loads = Vec::new();
    let mut dynamic = None;
    for i in 0..phnum {
        let ph = phoff.checked_add(i.checked_mul(phentsize)?)?;
        let offset = u64_at(data, ph + 8)?;
        let vaddr = u64_at(data, ph + 16)?;
        let filesz = u64_at(data, ph + 32)?;
        match u32_at(data, ph)? {
            PT_LOAD => loads.push((vaddr, offset, filesz)),
            PT_DYNAMIC => dynamic = Some((offset as usize, filesz as usize)),
            _ => {}
        }
    }
    let (dyn_off, dyn_size) = dynamic?;

    let mut strtab_vaddr = None;
    let mut needed_offsets = Vec::new();
    for i in 0..dyn_size / 16 {
        let at = dyn_off + i * 16;
        let tag = u64_at(data, at)?;
        let val = u64_at(data, at + 8)?;
        match tag {
            DT_NULL => break,
            DT_NEEDED => needed_offsets.push(val),
            DT_STRTAB => strtab_vaddr = Some(val),
            _ => {}
        }
    }
    let strtab_vaddr = strtab_vaddr?;
    let (vaddr, offset, _) = loads
        .iter()
        .find(|(v, _, size)| (*v..v + size).contains(&strtab_vaddr))?;
    let strtab = (strtab_vaddr - vaddr + offset) as usize;

    needed_offsets
        .iter()
        .map(|&o| {
            let start = strtab.checked_add(o as usize)?;
            let len = data.get(start..)?.iter().position(|&b| b == 0)?;
            String::from_utf8(data[start..start + len].to_vec()).ok()
        })
        .collect()
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn u64_at(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A minimal ELF64 file: header, two program headers (LOAD and DYNAMIC), a dynamic
    /// section and a string table, with the LOAD segment at a nonzero vaddr so the
    /// address translation is exercised.
    pub fn fake_elf(libs: &[&str]) -> Vec<u8> {
        let vbase = 0x400000u64;
        let phoff = 64u64;
        let dyn_off = phoff + 2 * 56;
        let dyn_count = libs.len() as u64 + 2;
        let str_off = dyn_off + dyn_count * 16;

        let mut strtab = vec![0u8];
        let mut name_offs = Vec::new();
        for l in libs {
            name_offs.push(strtab.len() as u64);
            strtab.extend_from_slice(l.as_bytes());
            strtab.push(0);
        }
        let total = str_off + strtab.len() as u64;

        let mut d = vec![0u8; 64];
        d[..4].copy_from_slice(MAGIC);
        d[4] = 2;
        d[5] = 1;
        d[0x20..0x28].copy_from_slice(&phoff.to_le_bytes());
        d[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
        d[0x38..0x3a].copy_from_slice(&2u16.to_le_bytes());

        let mut ph = |kind: u32, offset: u64, vaddr: u64, size: u64| {
            let mut p = vec![0u8; 56];
            p[0..4].copy_from_slice(&kind.to_le_bytes());
            p[8..16].copy_from_slice(&offset.to_le_bytes());
            p[16..24].copy_from_slice(&vaddr.to_le_bytes());
            p[32..40].copy_from_slice(&size.to_le_bytes());
            d.extend(p);
        };
        ph(PT_LOAD, 0, vbase, total);
        ph(PT_DYNAMIC, dyn_off, vbase + dyn_off, dyn_count * 16);

        let mut entry = |tag: u64, val: u64| {
            d.extend(tag.to_le_bytes());
            d.extend(val.to_le_bytes());
        };
        for o in &name_offs {
            entry(DT_NEEDED, *o);
        }
        entry(DT_STRTAB, vbase + str_off);
        entry(DT_NULL, 0);
        d.extend(strtab);
        d
    }

    #[test]
    fn reads_needed() {
        let elf = fake_elf(&["libnss3.so", "libgtk-3.so.0", "libc.so.6"]);
        assert_eq!(needed(&elf).unwrap(), ["libnss3.so", "libgtk-3.so.0", "libc.so.6"]);
        assert_eq!(needed(&fake_elf(&[])).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn refuses_what_it_cannot_read() {
        let elf = fake_elf(&["libx.so"]);
        let mut elf32 = elf.clone();
        elf32[4] = 1;
        let cases: &[&[u8]] = &[b"", b"\x7fELF", b"#!/bin/sh\n", &elf32, &elf[..100], &elf[..elf.len() - 3]];
        for (i, input) in cases.iter().enumerate() {
            assert_eq!(needed(input), None, "case {i}");
        }
    }
}
