//! Склейка кадров прокрутки в одну длинную картинку.
//!
//! Сдвиг между соседними кадрами ищется по подписям строк: строка нового кадра должна совпасть
//! со строкой прошлого, сдвинутой на искомое число пикселей. Строки, одинаковые в обоих кадрах
//! на своих местах сверху и снизу, считаются закреплёнными шапкой и подвалом и в поиске не участвуют.

use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo,
};

/// Сколько строк перекрытия нужно, чтобы поверить найденному сдвигу.
const MIN_OVERLAP: usize = 8;
/// Доля совпавших строк перекрытия, ниже которой сдвиг не принимается. Мигающий курсор
/// или анимация портят несколько строк, но не больше.
const MIN_MATCH: f64 = 0.95;

/// Кадр: пиксели BGRA по строкам сверху вниз.
#[derive(Clone)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

impl Frame {
    pub fn from_image(image: &CGImage) -> Option<Self> {
        let (width, height) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
        let mut pixels = vec![0u32; width * height];
        let space = CGImage::color_space(Some(image));
        // SAFETY: буфер на width * height пикселей по 4 байта живёт дольше контекста.
        let context = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                width,
                height,
                8,
                width * 4,
                space.as_deref(),
                CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0,
            )
        }?;
        let bounds = CGRect::new(CGPoint::ZERO, CGSize::new(width as f64, height as f64));
        CGContext::draw_image(Some(&context), bounds, Some(image));
        drop(context);
        Some(Frame { width, height, pixels })
    }

    fn row(&self, y: usize) -> &[u32] {
        &self.pixels[y * self.width..(y + 1) * self.width]
    }

    /// Тот же кадр вверх ногами. Прокрутка вверх склеивается как прокрутка вниз перевёрнутых кадров.
    pub fn flipped(&self) -> Self {
        let pixels = (0..self.height).rev().flat_map(|y| self.row(y).iter().copied()).collect();
        Frame { pixels, ..*self }
    }
}

/// Подпись строки и то, однотонная ли она. Однотонные строки совпадают где угодно и сдвиг не выдают.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Row {
    hash: u64,
    plain: bool,
}

pub fn rows(frame: &Frame) -> Vec<Row> {
    (0..frame.height)
        .map(|y| {
            let row = frame.row(y);
            // FNV-1a по пикселям.
            let hash = row.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, pixel| {
                (hash ^ u64::from(*pixel)).wrapping_mul(0x0000_0100_0000_01b3)
            });
            Row { hash, plain: row.iter().all(|pixel| *pixel == row[0]) }
        })
        .collect()
}

/// Как новый кадр лежит относительно прошлого.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Shift {
    /// На сколько строк содержимое уехало вверх. 0 - кадр не изменился.
    pub offset: usize,
    /// Закреплённые строки сверху и снизу.
    pub header: usize,
    pub footer: usize,
}

/// Ищет, на сколько строк уехало содержимое между кадрами. None - совпадения не нашлось.
/// Сдвиг без единой несовпавшей строки лучше остальных. Из одинаково хороших `hint` выбирает
/// ближайший к ожидаемому сдвигу, а без него берётся сдвиг с самым длинным перекрытием.
pub fn find_shift(previous: &[Row], next: &[Row], hint: Option<usize>) -> Option<Shift> {
    let height = previous.len().min(next.len());
    let header = (0..height).take_while(|y| previous[*y].hash == next[*y].hash).count();
    if header == height {
        return Some(Shift { offset: 0, header, footer: 0 });
    }
    let footer = (header..height).rev().take_while(|y| previous[*y].hash == next[*y].hash).count();
    let bottom = height - footer;
    let mut best: Option<(usize, usize, bool)> = None;
    for offset in 1..bottom - header {
        // Строка y нового кадра - это строка y + offset прошлого.
        let (mut seen, mut matched) = (0, 0);
        for y in header..bottom - offset {
            if next[y].plain {
                continue;
            }
            seen += 1;
            if next[y].hash == previous[y + offset].hash {
                matched += 1;
            }
        }
        if seen < MIN_OVERLAP || (matched as f64) < MIN_MATCH * seen as f64 {
            continue;
        }
        let perfect = matched == seen;
        let better = match (best, hint) {
            (None, _) => true,
            (Some((.., best_perfect)), _) if perfect != best_perfect => perfect,
            (Some((best_offset, ..)), Some(hint)) => offset.abs_diff(hint) < best_offset.abs_diff(hint),
            (Some((_, best_matched, _)), None) => matched > best_matched,
        };
        if better {
            best = Some((offset, matched, perfect));
        }
    }
    best.map(|(offset, ..)| Shift { offset, header, footer })
}

/// Что сделал новый кадр.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Push {
    /// Добавил столько строк.
    Added(usize),
    /// Не сдвинулся: прокрутка дошла до конца или ещё не началась.
    Same,
    /// Не нашлось перекрытия с прошлым кадром, кадр пропущен.
    Lost,
}

/// Длинная картинка, которая растёт снизу.
pub struct Stitcher {
    width: usize,
    /// Всё склеенное, внизу всегда низ последнего кадра.
    pixels: Vec<u32>,
    last: Vec<Row>,
    frame_height: usize,
}

impl Stitcher {
    pub fn new(first: &Frame) -> Self {
        Stitcher { width: first.width, pixels: first.pixels.clone(), last: rows(first), frame_height: first.height }
    }

    pub fn height(&self) -> usize {
        self.pixels.len() / self.width
    }

    pub fn push(&mut self, frame: &Frame, hint: Option<usize>) -> Push {
        if frame.width != self.width || frame.height != self.frame_height {
            return Push::Lost;
        }
        let next = rows(frame);
        let Some(shift) = find_shift(&self.last, &next, hint) else { return Push::Lost };
        if shift.offset == 0 {
            return Push::Same;
        }
        // Подвал прошлого кадра уходит, на его место встают новые строки и подвал этого кадра.
        let footer = shift.footer;
        self.pixels.truncate((self.height() - footer) * self.width);
        let from = (frame.height - footer - shift.offset) * self.width;
        self.pixels.extend_from_slice(&frame.pixels[from..]);
        self.last = next;
        Push::Added(shift.offset)
    }

    /// Склеенная картинка. `flipped` - кадры переворачивали для прокрутки вверх.
    pub fn image(&self, flipped: bool) -> Option<CFRetained<CGImage>> {
        let height = self.height();
        let mut pixels = if flipped {
            (0..height).rev().flat_map(|y| self.pixels[y * self.width..(y + 1) * self.width].iter().copied()).collect()
        } else {
            self.pixels.clone()
        };
        let space = CGColorSpace::new_device_rgb();
        // SAFETY: буфер на width * height пикселей по 4 байта живёт дольше контекста, а картинка копирует его.
        let context = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                self.width,
                height,
                8,
                self.width * 4,
                space.as_deref(),
                CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0,
            )
        }?;
        CGBitmapContextCreateImage(Some(&context))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Высокая «страница», где у каждой строки свой рисунок, с однотонными полями между абзацами.
    fn page(width: usize, height: usize) -> Vec<u32> {
        (0..height)
            .flat_map(|y| {
                (0..width).map(move |x| {
                    if y % 40 >= 30 {
                        0xffff_ffff
                    } else {
                        0xff00_0000 | ((y as u32).wrapping_mul(2_654_435_761) ^ x as u32)
                    }
                })
            })
            .collect()
    }

    /// Окно высотой `height` на странице с позиции `top`, с закреплённой шапкой и подвалом.
    fn window(page: &[u32], width: usize, top: usize, height: usize, header: usize, footer: usize) -> Frame {
        let mut pixels = page[top * width..(top + height) * width].to_vec();
        pixels[..header * width].fill(0xff11_2233);
        pixels[(height - footer) * width..].fill(0xff44_5566);
        Frame { width, height, pixels }
    }

    #[test]
    fn shift_is_found_past_a_fixed_header_and_footer() {
        let (width, height) = (16, 120);
        let page = page(width, 600);
        let first = window(&page, width, 0, height, 10, 6);
        let second = window(&page, width, 37, height, 10, 6);
        let shift = find_shift(&rows(&first), &rows(&second), None).unwrap();
        // над подвалом может оказаться совпавшая однотонная строка, она тоже считается подвалом
        assert_eq!((shift.offset, shift.header), (37, 10));
        assert!(shift.footer >= 6);
        assert_eq!(find_shift(&rows(&first), &rows(&first), None).unwrap().offset, 0);
        // кадр с другой страницы не совпадает
        let other: Vec<u32> = page.iter().map(|p| p ^ 0x00ff_00ff).collect();
        assert_eq!(find_shift(&rows(&first), &rows(&window(&other, width, 300, height, 10, 6)), None), None);
    }

    #[test]
    fn stitched_page_equals_the_original_between_header_and_footer() {
        let (width, height, header, footer) = (12, 100, 8, 5);
        let page = page(width, 400);
        let tops = [0, 30, 75, 75, 140, 210, 270];
        let mut stitcher = Stitcher::new(&window(&page, width, tops[0], height, header, footer));
        let mut pushes = Vec::new();
        for top in &tops[1..] {
            pushes.push(stitcher.push(&window(&page, width, *top, height, header, footer), None));
        }
        assert_eq!(
            pushes,
            [Push::Added(30), Push::Added(45), Push::Same, Push::Added(65), Push::Added(70), Push::Added(60)]
        );
        // шапка, вся страница от её низа до низа последнего окна над подвалом, подвал
        let total = 270 + height;
        assert_eq!(stitcher.height(), total);
        let pixels = &stitcher.pixels;
        assert!(pixels[..header * width].iter().all(|p| *p == 0xff11_2233));
        assert_eq!(&pixels[header * width..(total - footer) * width], &page[header * width..(total - footer) * width]);
        assert!(pixels[(total - footer) * width..].iter().all(|p| *p == 0xff44_5566));
    }

    #[test]
    fn scrolling_up_stitches_flipped_frames() {
        let (width, height) = (10, 80);
        let page = page(width, 300);
        let mut stitcher = Stitcher::new(&window(&page, width, 200, height, 0, 0).flipped());
        assert_eq!(stitcher.push(&window(&page, width, 150, height, 0, 0).flipped(), None), Push::Added(50));
        assert_eq!(stitcher.push(&window(&page, width, 90, height, 0, 0).flipped(), None), Push::Added(60));
        let image = stitcher.image(true).unwrap();
        let frame = Frame::from_image(&image).unwrap();
        assert_eq!(frame.height, 190);
        assert_eq!(frame.pixels, page[90 * width..280 * width]);
    }

    #[test]
    fn rendered_text_frames_shift_by_the_scrolled_amount() {
        let frame = |bytes: &[u8]| Frame::from_image(&crate::output::decode(bytes).unwrap().0).unwrap();
        let first = frame(include_bytes!("../tests/fixtures/scroll-1.png"));
        let second = frame(include_bytes!("../tests/fixtures/scroll-2.png"));
        // 173 точки при масштабе 2. Шапка 50 точек и белые строки под ней одинаковы в обоих кадрах.
        let shift = find_shift(&rows(&first), &rows(&second), None).unwrap();
        assert_eq!(shift.offset, 346);
        assert!(shift.header >= 100);
        let mut stitcher = Stitcher::new(&first);
        assert_eq!(stitcher.push(&second, None), Push::Added(346));
        assert_eq!(stitcher.height(), 800 + 346);
    }

    #[test]
    fn hint_picks_the_expected_shift_on_repeating_content() {
        // строки списка повторяются через 20 пикселей, сдвиг 30 неотличим от 10, 50 и 70
        let width = 8;
        let list: Vec<u32> =
            (0..400).flat_map(|y| (0..width).map(move |x| 0xff00_0000 | ((y % 20) * 31 + x) as u32)).collect();
        let first = Frame { width, height: 100, pixels: list[..100 * width].to_vec() };
        let second = Frame { width, height: 100, pixels: list[30 * width..130 * width].to_vec() };
        let (first, second) = (rows(&first), rows(&second));
        assert_eq!(find_shift(&first, &second, Some(28)).unwrap().offset, 30);
        assert_eq!(find_shift(&first, &second, Some(52)).unwrap().offset, 50);
        assert_eq!(find_shift(&first, &second, None).unwrap().offset, 10);
    }
}
