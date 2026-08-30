//! Perceptuele hash om onveranderde schermen te herkennen.
//!
//! Verreweg de meeste frames zijn identiek aan het vorige: je leest een
//! pagina, je denkt na, een dialoog staat open. Die frames door OCR halen is
//! puur verspilling, dus vergelijken we eerst een 64-bits vingerafdruk.

use image::RgbaImage;

/// Difference hash: schaal naar 9x8 grijswaarden en vergelijk elke pixel met
/// zijn rechterbuur. Ongevoelig voor helderheid en kleine ruis, gevoelig voor
/// echte layoutveranderingen.
pub fn dhash(image: &RgbaImage) -> u64 {
    let small = image::imageops::thumbnail(image, 9, 8);
    let mut bits: u64 = 0;

    for y in 0..8u32 {
        for x in 0..8u32 {
            let left = luma(small.get_pixel(x, y).0);
            let right = luma(small.get_pixel(x + 1, y).0);
            if left > right {
                bits |= 1 << (y * 8 + x);
            }
        }
    }
    bits
}

fn luma([r, g, b, _]: [u8; 4]) -> u32 {
    // Rec. 601, in gehele getallen om afronding voorspelbaar te houden.
    299 * r as u32 + 587 * g as u32 + 114 * b as u32
}

/// Aantal verschillende bits: 0 = identiek, 64 = compleet ander beeld.
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn gradient(offset: u8) -> RgbaImage {
        RgbaImage::from_fn(64, 64, |x, y| {
            let v = ((x * 4 + y * 2) as u8).wrapping_add(offset);
            Rgba([v, v, v, 255])
        })
    }

    #[test]
    fn identieke_beelden_hebben_afstand_nul() {
        assert_eq!(hamming(dhash(&gradient(0)), dhash(&gradient(0))), 0);
    }

    #[test]
    fn helderheidsverschil_verandert_de_hash_nauwelijks() {
        // Een uniforme verschuiving is geen inhoudelijke verandering.
        let d = hamming(dhash(&gradient(0)), dhash(&gradient(10)));
        assert!(d <= 4, "afstand {d} is te groot voor alleen helderheid");
    }

    #[test]
    fn ander_beeld_geeft_grote_afstand() {
        let vlak = RgbaImage::from_pixel(64, 64, Rgba([20, 20, 20, 255]));
        let d = hamming(dhash(&gradient(0)), dhash(&vlak));
        assert!(d > 8, "afstand {d} is te klein voor een ander beeld");
    }
}
