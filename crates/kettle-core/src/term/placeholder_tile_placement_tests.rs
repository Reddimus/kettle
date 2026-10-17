use super::{VirtualEntry, placeholder_tile_placement};
use kettle_vt::ImageData;
use kettle_vt::placeholder::ResolvedCell;

#[test]
fn placeholder_tile_shares_pixels_and_records_source_rect() {
    let image = ImageData::new(4, 2, vec![0; 4 * 2 * 4]).expect("test image");
    let virtual_image = VirtualEntry {
        img: image.clone(),
        placement_id: 0,
        cols: 2,
        rows: 1,
        z: 7,
    };
    let placement = placeholder_tile_placement(
        12,
        3,
        ResolvedCell {
            image_id: 42,
            placement_id: 0,
            row: 0,
            col: 1,
        },
        &virtual_image,
    )
    .expect("right-hand tile");

    assert_eq!(placement.img.allocation_key(), image.allocation_key());
    assert_eq!(placement.img.byte_len(), image.byte_len());
    assert_eq!(
        placement.source_rect,
        Some(crate::ImageSourceRect {
            x: 2,
            y: 0,
            width: 2,
            height: 2,
        })
    );
}
