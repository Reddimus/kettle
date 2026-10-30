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
            x: 2.0,
            y: 0.0,
            width: 2.0,
            height: 2.0,
        })
    );
}

#[test]
fn enlarged_placeholder_preserves_four_fractional_rectangles() {
    let image = ImageData::new(1, 1, vec![1, 2, 3, 255]).unwrap();
    let virtual_image = VirtualEntry {
        img: image.clone(),
        placement_id: 0,
        cols: 2,
        rows: 2,
        z: 0,
    };
    let mut rects = Vec::new();
    for row in 0..2 {
        for col in 0..2 {
            let tile = placeholder_tile_placement(
                0,
                col as usize,
                ResolvedCell {
                    image_id: 7,
                    placement_id: 0,
                    row,
                    col,
                },
                &virtual_image,
            )
            .unwrap();
            assert_eq!(tile.img.allocation_key(), image.allocation_key());
            let rect = tile.source_rect.unwrap();
            assert!(rect.width > 0.0 && rect.height > 0.0);
            rects.push((rect.x, rect.y, rect.width, rect.height));
        }
    }
    assert_eq!(
        rects,
        [
            (0.0, 0.0, 0.5, 0.5),
            (0.5, 0.0, 0.5, 0.5),
            (0.0, 0.5, 0.5, 0.5),
            (0.5, 0.5, 0.5, 0.5)
        ]
    );
}
