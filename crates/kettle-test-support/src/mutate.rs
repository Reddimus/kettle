//! A small, seeded mutation engine for the media parsers' in-process sweeps:
//! each case is a seed with one to four mutations of the kinds fuzzers find
//! bugs with (a flipped bit, an edge value in a byte or a length field, a
//! cut, a repeated or inserted run, a splice of another seed). The same
//! `seed` gives the same cases on every machine, so a failure names its
//! case and comes back on a rerun.
//!
//! The sweeps' seeds are also the fuzz targets' starting corpus: with
//! `KETTLE_FUZZ_CORPUS` naming a directory, [`save_seeds`] writes each
//! sweep's seeds there under its target's name (see
//! `.github/workflows/fuzz.yml`).

use std::path::Path;

/// xorshift64*: enough randomness to spread mutations, and reproducible.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value below `bound`, which must not be zero.
    pub fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }
}

/// Byte values parsers trip on.
const EDGE_BYTES: [u8; 6] = [0x00, 0x01, 0x7F, 0x80, 0xFE, 0xFF];
/// Little-endian length and count values parsers trip on: zero, one, the
/// signed and unsigned edges, and sizes just past common caps.
const EDGE_WORDS: [u32; 8] = [
    0,
    1,
    0x7FFF_FFFF,
    0x8000_0000,
    0xFFFF_FFFF,
    0x0100_0000,
    0x0040_0001,
    0x0000_FFFF,
];

/// `count` cases built from `seeds` with `seed`'s randomness: the seeds
/// themselves first, then mutations of them.
pub fn cases(seeds: &[&[u8]], count: usize, seed: u64) -> Vec<Vec<u8>> {
    let mut rng = Rng::new(seed);
    let mut cases: Vec<Vec<u8>> = seeds.iter().map(|seed| seed.to_vec()).collect();
    if seeds.is_empty() {
        return cases;
    }
    while cases.len() < count {
        let mut case = seeds[rng.below(seeds.len())].to_vec();
        for _ in 0..=rng.below(4) {
            mutate(&mut case, seeds, &mut rng);
        }
        cases.push(case);
    }
    cases
}

/// Write `seeds` as fuzz target `target`'s corpus under the directory
/// `KETTLE_FUZZ_CORPUS` names, when it names one.
pub fn save_seeds(target: &str, seeds: &[&[u8]]) {
    if let Some(root) = std::env::var_os("KETTLE_FUZZ_CORPUS") {
        save_seeds_to(Path::new(&root), target, seeds);
    }
}

/// Write `seeds` into `root`/`target`, one file each, named by place.
pub fn save_seeds_to(root: &Path, target: &str, seeds: &[&[u8]]) {
    let directory = root.join(target);
    std::fs::create_dir_all(&directory).expect("create the corpus directory");
    for (index, seed) in seeds.iter().enumerate() {
        std::fs::write(directory.join(format!("seed-{index:03}")), seed).expect("write a seed");
    }
}

/// Apply one mutation to `case`.
pub fn mutate(case: &mut Vec<u8>, seeds: &[&[u8]], rng: &mut Rng) {
    match rng.below(7) {
        0 if !case.is_empty() => {
            let at = rng.below(case.len());
            case[at] ^= 1 << rng.below(8);
        }
        1 if !case.is_empty() => {
            let at = rng.below(case.len());
            case[at] = EDGE_BYTES[rng.below(EDGE_BYTES.len())];
        }
        2 if case.len() >= 4 => {
            let at = rng.below(case.len() - 3);
            let word = EDGE_WORDS[rng.below(EDGE_WORDS.len())];
            case[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
        3 if !case.is_empty() => {
            case.truncate(rng.below(case.len()));
        }
        4 if !case.is_empty() => {
            let start = rng.below(case.len());
            let len = rng.below((case.len() - start).min(64)) + 1;
            let run = case[start..start + len].to_vec();
            let at = rng.below(case.len() + 1);
            case.splice(at..at, run);
        }
        5 => {
            let at = rng.below(case.len() + 1);
            let len = rng.below(16) + 1;
            let run: Vec<u8> = (0..len).map(|_| rng.next_u64() as u8).collect();
            case.splice(at..at, run);
        }
        _ if !seeds.is_empty() => {
            let other = seeds[rng.below(seeds.len())];
            if !other.is_empty() && !case.is_empty() {
                let cut = rng.below(case.len());
                let from = rng.below(other.len());
                case.truncate(cut);
                case.extend_from_slice(&other[from..]);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same seed gives the same cases, the seeds come first, every case
    /// differs from its seed somewhere across a run, and an empty seed list
    /// gives nothing.
    #[test]
    fn cases_are_reproducible_and_mutated() {
        let seeds: [&[u8]; 2] = [
            b"KMED\x06\x00\x0c\x01\x00\x00\x00\x00",
            b"\x89PNG\r\n\x1a\n",
        ];
        let first = cases(&seeds, 200, 7);
        assert_eq!(first, cases(&seeds, 200, 7));
        assert_ne!(first, cases(&seeds, 200, 8));
        assert_eq!(first.len(), 200);
        assert_eq!(first[0], seeds[0]);
        assert_eq!(first[1], seeds[1]);
        let changed = first[2..]
            .iter()
            .filter(|case| !seeds.contains(&case.as_slice()))
            .count();
        assert!(changed > 150, "only {changed} cases changed");
        assert!(cases(&[], 10, 1).is_empty());
        // Edge words reach the cases.
        assert!(
            first
                .iter()
                .any(|case| case.windows(4).any(|word| word == [0xFF; 4]))
        );
    }

    /// An empty case with no seeds to splice from stays an empty or short
    /// case: no mutation panics for want of input.
    #[test]
    fn mutating_nothing_never_panics() {
        let mut rng = Rng::new(1);
        for _ in 0..1000 {
            let mut case = Vec::new();
            mutate(&mut case, &[], &mut rng);
            assert!(case.len() <= 16);
        }
    }

    /// Seeds are written one file each under their target, byte for byte.
    #[test]
    fn seeds_are_saved_under_their_target() {
        let root = crate::private_tempdir("kettle-fuzz-corpus-");
        let seeds: [&[u8]; 2] = [b"\x00KMED", b""];
        save_seeds_to(root.path(), "wire", &seeds);
        let directory = root.path().join("wire");
        assert_eq!(std::fs::read(directory.join("seed-000")).unwrap(), seeds[0]);
        assert_eq!(std::fs::read(directory.join("seed-001")).unwrap(), seeds[1]);
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 2);
    }
}
