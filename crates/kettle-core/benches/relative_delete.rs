//! Core-registry benchmark for Kitty relative-placement deletion.
//!
//! This is separate from kettle-vt's decoder-state benchmark. The fixture has
//! the same `RelEntry` map shape used by the terminal reader and synchronized
//! graphics replay paths. Setup is outside the timed section.

use std::collections::HashMap;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use kettle_core::images::{RelEntry, cascade_removed_relatives};
use kettle_core::{ImageData, PlacementParams};
use kettle_vt::kitty::PlacementKey;

fn key(image_id: u32, placement_id: u32) -> PlacementKey {
    PlacementKey {
        image_id,
        placement_id,
    }
}

fn maximum_depth_core_registry() -> HashMap<(u32, u32), RelEntry> {
    let image = ImageData::new(1, 1, vec![1, 2, 3, 4]).expect("benchmark pixel");
    let cap = kettle_vt::GraphicsLimits::default().placements as u32;
    (2..=cap + 1)
        .map(|placement| {
            (
                (1, placement),
                RelEntry {
                    img: image.clone(),
                    parent_img: 1,
                    parent_placement: placement - 1,
                    h: 0,
                    v: 0,
                    z: 0,
                    params: PlacementParams::default(),
                },
            )
        })
        .collect()
}

fn bench_core_relative_delete(c: &mut Criterion) {
    c.bench_function("kitty_delete/core_registry_max_depth_relative_chain", |b| {
        b.iter_batched(
            maximum_depth_core_registry,
            |mut relatives| {
                let mut removed_keys = std::collections::HashSet::from([key(1, 1)]);
                let mut removed_ids = std::collections::HashSet::from([1]);
                cascade_removed_relatives(&mut relatives, &mut removed_keys, &mut removed_ids);
                assert!(relatives.is_empty());
                relatives.len()
            },
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(benches, bench_core_relative_delete);
criterion_main!(benches);
