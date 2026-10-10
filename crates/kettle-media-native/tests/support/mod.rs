//! Reading the video fixtures' frames back.

/// The fixture frame a decoded tile shows. Each frame carries its index in
/// six blocks, three across and two down, bits 0 to 5 left to right and top
/// to bottom, white for a one. `turned` reads a tile shown turned a quarter
/// counterclockwise, as `rotated.mp4` is: a coded point (x, y) shows at
/// (y, 1 - x).
pub fn frame_index(rgba: &[u8], width: u32, height: u32, turned: bool) -> u32 {
    (0..6).fold(0, |index, bit| {
        let (x, y) = (
            (f64::from(bit % 3) + 0.5) / 3.0,
            (f64::from(bit / 3) + 0.5) / 2.0,
        );
        let (x, y) = if turned { (y, 1.0 - x) } else { (x, y) };
        let column = ((x * f64::from(width)) as u32).min(width - 1);
        let row = ((y * f64::from(height)) as u32).min(height - 1);
        let red = rgba[((row * width + column) * 4) as usize];
        if red > 128 { index | 1 << bit } else { index }
    })
}
