//! Synthetic outline fixtures derived in memory from the bundled OFL font.
//! Primary names are changed; copyright/license records are retained. The
//! fixtures test selection and coverage, not authentic non-Latin typography.

use std::collections::BTreeMap;

pub const FAMILY: &str = "Kettle Test Fallback";
const FONT: &[u8] = include_bytes!("../../../../assets/fonts/JetBrainsMonoNerdFont-Regular.ttf");

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes(bytes[at..at + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}

pub fn tables() -> BTreeMap<[u8; 4], Vec<u8>> {
    (0..usize::from(u16_at(FONT, 4)))
        .map(|i| {
            let at = 12 + 16 * i;
            let start = u32_at(FONT, at + 8) as usize;
            let end = start + u32_at(FONT, at + 12) as usize;
            (
                FONT[at..at + 4].try_into().unwrap(),
                FONT[start..end].to_vec(),
            )
        })
        .collect()
}

fn checksum(bytes: &[u8]) -> u32 {
    bytes.chunks(4).fold(0, |sum: u32, chunk| {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

pub fn sfnt(mut tables: BTreeMap<[u8; 4], Vec<u8>>) -> Vec<u8> {
    tables.get_mut(b"head").unwrap()[8..12].fill(0);
    let count = u16::try_from(tables.len()).unwrap();
    let power = 1u16 << count.ilog2();
    let mut bytes = 0x0001_0000u32.to_be_bytes().to_vec();
    for value in [
        count,
        power * 16,
        power.ilog2() as u16,
        count * 16 - power * 16,
    ] {
        bytes.extend(value.to_be_bytes());
    }
    bytes.resize(12 + 16 * tables.len(), 0);
    let mut head_offset = 0;
    for (i, (tag, data)) in tables.into_iter().enumerate() {
        let offset = bytes.len();
        let at = 12 + 16 * i;
        bytes[at..at + 4].copy_from_slice(&tag);
        bytes[at + 4..at + 8].copy_from_slice(&checksum(&data).to_be_bytes());
        bytes[at + 8..at + 12].copy_from_slice(&(offset as u32).to_be_bytes());
        bytes[at + 12..at + 16].copy_from_slice(&(data.len() as u32).to_be_bytes());
        if &tag == b"head" {
            head_offset = offset;
        }
        bytes.extend(data);
        bytes.resize(bytes.len().next_multiple_of(4), 0);
    }
    let adjustment = 0xb1b0_afbau32.wrapping_sub(checksum(&bytes));
    bytes[head_offset + 8..head_offset + 12].copy_from_slice(&adjustment.to_be_bytes());
    bytes
}

fn rename(name: &[u8]) -> Vec<u8> {
    let count = usize::from(u16_at(name, 2));
    let strings = usize::from(u16_at(name, 4));
    let mut records = name[6..6 + 12 * count].to_vec();
    let mut data = Vec::new();
    for record in records.as_chunks_mut::<12>().0 {
        let id = u16_at(record, 6);
        let old_start = strings + usize::from(u16_at(record, 10));
        let old_end = old_start + usize::from(u16_at(record, 8));
        let text = match id {
            1 | 4 | 16 | 21 => Some(FAMILY),
            3 | 6 => Some("KettleTestFallback"),
            _ => None,
        };
        let value = if let Some(text) = text {
            if matches!(u16_at(record, 0), 0 | 3) {
                text.encode_utf16().flat_map(u16::to_be_bytes).collect()
            } else {
                text.as_bytes().to_vec()
            }
        } else {
            name[old_start..old_end].to_vec()
        };
        record[8..10].copy_from_slice(&(value.len() as u16).to_be_bytes());
        record[10..12].copy_from_slice(&(data.len() as u16).to_be_bytes());
        data.extend(value);
    }
    let mut result = vec![0, 0];
    result.extend((count as u16).to_be_bytes());
    result.extend((6 + records.len() as u16).to_be_bytes());
    result.extend(records);
    result.extend(data);
    result
}

fn a_glyph(cmap: &[u8]) -> u32 {
    for record in cmap[4..4 + usize::from(u16_at(cmap, 2)) * 8]
        .as_chunks::<8>()
        .0
    {
        let at = u32_at(record, 4) as usize;
        if u16_at(cmap, at) != 12 {
            continue;
        }
        for i in 0..u32_at(cmap, at + 12) as usize {
            let group = at + 16 + 12 * i;
            let start = u32_at(cmap, group);
            if start <= u32::from('A') && u32::from('A') <= u32_at(cmap, group + 4) {
                return u32_at(cmap, group + 8) + u32::from('A') - start;
            }
        }
    }
    panic!("the bundled Nerd Font has a format-12 map for A");
}

fn cmap(glyph: u32, extra: Option<char>) -> Vec<u8> {
    let mut characters = vec!['A'];
    characters.extend(extra);
    characters.sort_unstable();
    let mut bytes = vec![0, 0, 0, 1, 0, 3, 0, 10];
    bytes.extend(12u32.to_be_bytes());
    bytes.extend([0, 12, 0, 0]);
    bytes.extend((16 + characters.len() as u32 * 12).to_be_bytes());
    bytes.extend(0u32.to_be_bytes());
    bytes.extend((characters.len() as u32).to_be_bytes());
    for character in characters {
        bytes.extend(u32::from(character).to_be_bytes());
        bytes.extend(u32::from(character).to_be_bytes());
        bytes.extend(glyph.to_be_bytes());
    }
    bytes
}

pub fn single(extra: Option<char>, wide: bool) -> Vec<u8> {
    let mut tables = tables();
    let glyph = a_glyph(&tables[b"cmap"]);
    let name = rename(&tables[b"name"]);
    tables.insert(*b"name", name);
    tables.insert(*b"cmap", cmap(glyph, extra));
    if wide {
        let metrics = usize::from(u16_at(&tables[b"hhea"], 34));
        let at = (glyph as usize).min(metrics - 1) * 4;
        let advance = u16_at(&tables[b"hmtx"], at).checked_mul(2).unwrap();
        tables.get_mut(b"hmtx").unwrap()[at..at + 2].copy_from_slice(&advance.to_be_bytes());
    }
    sfnt(tables)
}

pub fn collection() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let first = single(None, false);
    let second = single(Some('\u{4e2d}'), true);
    let mut collection = b"ttcf".to_vec();
    collection.extend(0x0001_0000u32.to_be_bytes());
    collection.extend(2u32.to_be_bytes());
    collection.extend(20u32.to_be_bytes());
    collection.extend((20 + first.len() as u32).to_be_bytes());
    for face in [&first, &second] {
        let base = collection.len();
        let mut face = face.clone();
        for i in 0..usize::from(u16_at(&face, 4)) {
            let at = 12 + 16 * i + 8;
            let offset = u32_at(&face, at) + base as u32;
            face[at..at + 4].copy_from_slice(&offset.to_be_bytes());
        }
        collection.extend(face);
    }
    (collection, first, second)
}
