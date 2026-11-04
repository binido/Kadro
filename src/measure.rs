//! Измерения по пикселям снимка: цвет точки, его запись для копирования и границы ровного участка.
//!
//! Пиксели читаются в sRGB, как их показывают веб-инструменты и «Цифровой колориметр».

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo, kCGColorSpaceSRGB,
};

/// Записи цвета для выпадающего списка пипетки, по номеру варианта.
pub const FORMATS: [&str; 3] = ["HEX", "RGB", "HSL"];

/// Насколько может отличаться канал соседнего пикселя, чтобы он считался тем же цветом.
const EDGE_THRESHOLD: u8 = 16;

/// Цвет одного пикселя. Начало координат вверху слева.
pub fn pixel(image: &CGImage, x: usize, y: usize) -> Option<[u8; 3]> {
    strip(image, x, y, 1, 1)?.first().copied()
}

/// Строка пикселей по всей ширине картинки.
pub fn row(image: &CGImage, y: usize) -> Option<Vec<[u8; 3]>> {
    strip(image, 0, y, CGImage::width(Some(image)), 1)
}

/// Столбец пикселей по всей высоте картинки.
pub fn column(image: &CGImage, x: usize) -> Option<Vec<[u8; 3]>> {
    strip(image, x, 0, 1, CGImage::height(Some(image)))
}

/// Пиксели прямоугольника в sRGB по строкам сверху вниз.
fn strip(image: &CGImage, x: usize, y: usize, width: usize, height: usize) -> Option<Vec<[u8; 3]>> {
    if x + width > CGImage::width(Some(image)) || y + height > CGImage::height(Some(image)) {
        return None;
    }
    let rect = CGRect::new(CGPoint::new(x as f64, y as f64), CGSize::new(width as f64, height as f64));
    let part = CGImage::with_image_in_rect(Some(image), rect)?;
    let mut bytes = vec![0u8; width * height * 4];
    // SAFETY: системная константа имени цветового пространства живёт весь процесс.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
    // SAFETY: буфер на width * height пикселей по 4 байта живёт дольше контекста.
    let context = unsafe {
        CGBitmapContextCreate(
            bytes.as_mut_ptr().cast(),
            width,
            height,
            8,
            width * 4,
            space.as_deref(),
            CGImageAlphaInfo::NoneSkipLast.0,
        )
    }?;
    let bounds = CGRect::new(CGPoint::ZERO, CGSize::new(width as f64, height as f64));
    CGContext::draw_image(Some(&context), bounds, Some(&part));
    drop(context);
    Some(bytes.chunks_exact(4).map(|p| [p[0], p[1], p[2]]).collect())
}

/// Цвет текстом для буфера обмена: `#RRGGBB`, `rgb(r, g, b)` или `hsl(h, s%, l%)`.
pub fn format(color: [u8; 3], format: usize) -> String {
    let [red, green, blue] = color;
    match format {
        1 => format!("rgb({red}, {green}, {blue})"),
        2 => {
            let (hue, saturation, lightness) = hsl(color);
            format!("hsl({hue:.0}, {saturation:.0}%, {lightness:.0}%)")
        }
        _ => format!("#{red:02X}{green:02X}{blue:02X}"),
    }
}

/// Тон в градусах, насыщенность и светлота в процентах.
fn hsl(color: [u8; 3]) -> (f64, f64, f64) {
    let [r, g, b] = color.map(|c| f64::from(c) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let lightness = (max + min) / 2.0;
    let delta = max - min;
    if delta == 0.0 {
        return (0.0, 0.0, lightness * 100.0);
    }
    let saturation = delta / (1.0 - (2.0 * lightness - 1.0).abs());
    let sector = if max == r {
        ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    (sector * 60.0, saturation * 100.0, lightness * 100.0)
}

/// Границы участка того же цвета, что и пиксель `at`, включительно.
pub fn span(line: &[[u8; 3]], at: usize) -> (usize, usize) {
    let reference = line[at];
    let same = |p: &[u8; 3]| p.iter().zip(reference).all(|(a, b)| a.abs_diff(b) <= EDGE_THRESHOLD);
    let start = line[..at].iter().rposition(|p| !same(p)).map_or(0, |i| i + 1);
    let end = line[at..].iter().position(|p| !same(p)).map_or(line.len() - 1, |i| at + i - 1);
    (start, end)
}

/// Подпись линейки: длина в пикселях или в точках экрана, где сняли снимок.
pub fn ruler_label(length: f64, scale: f64, style: usize) -> String {
    if style == 1 {
        let points = length / scale;
        if (points - points.round()).abs() < 0.05 { format!("{points:.0} pt") } else { format!("{points:.1} pt") }
    } else {
        format!("{:.0} px", length.round())
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_graphics::{CGBitmapContextCreateImage, CGImageByteOrderInfo};

    use super::*;

    /// sRGB-картинка из заданных пикселей.
    fn image(width: usize, height: usize, pixels: &[[u8; 3]]) -> objc2_core_foundation::CFRetained<CGImage> {
        let mut bytes: Vec<u8> = pixels.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        // SAFETY: системная константа имени цветового пространства живёт весь процесс.
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
        // SAFETY: буфер на width * height пикселей по 4 байта живёт до конца функции, а картинка копирует его.
        let context = unsafe {
            CGBitmapContextCreate(
                bytes.as_mut_ptr().cast(),
                width,
                height,
                8,
                width * 4,
                space.as_deref(),
                CGImageAlphaInfo::NoneSkipLast.0 | CGImageByteOrderInfo::OrderDefault.0,
            )
        }
        .unwrap();
        CGBitmapContextCreateImage(Some(&context)).unwrap()
    }

    #[test]
    fn picked_pixel_matches_the_image_data() {
        let pixels: Vec<[u8; 3]> = (0..12u8).map(|i| [i * 20, 255 - i * 7, i * 3 + 1]).collect();
        let source = image(4, 3, &pixels);
        for y in 0..3 {
            for x in 0..4 {
                assert_eq!(pixel(&source, x, y), Some(pixels[y * 4 + x]), "({x}, {y})");
            }
        }
        assert_eq!(pixel(&source, 4, 0), None);
        assert_eq!(column(&source, 2).unwrap(), vec![pixels[2], pixels[6], pixels[10]]);
    }

    #[test]
    fn colour_is_written_as_hex_rgb_and_hsl() {
        assert_eq!(format([51, 102, 153], 0), "#336699");
        assert_eq!(format([51, 102, 153], 1), "rgb(51, 102, 153)");
        assert_eq!(format([51, 102, 153], 2), "hsl(210, 50%, 40%)");
        assert_eq!(format([255, 0, 0], 2), "hsl(0, 100%, 50%)");
        assert_eq!(format([0, 128, 0], 2), "hsl(120, 100%, 25%)");
        assert_eq!(format([255, 255, 255], 2), "hsl(0, 0%, 100%)");
        assert_eq!(format([255, 0, 128], 2), "hsl(330, 100%, 50%)");
    }

    #[test]
    fn span_stops_at_the_edges_around_the_point() {
        let white = [255, 255, 255];
        let black = [0, 0, 0];
        // антиалиасинг в пределах порога не считается краем
        let line = [black, white, [250, 250, 250], white, white, black, black];
        assert_eq!(span(&line, 3), (1, 4));
        assert_eq!(span(&line, 5), (5, 6));
        assert_eq!(span(&line, 0), (0, 0));
        assert_eq!(span(&[white; 4], 2), (0, 3));
    }

    #[test]
    fn ruler_shows_pixels_or_points() {
        assert_eq!(ruler_label(200.4, 2.0, 0), "200 px");
        assert_eq!(ruler_label(200.0, 2.0, 1), "100 pt");
        assert_eq!(ruler_label(201.0, 2.0, 1), "100.5 pt");
    }
}
