//! Editor document: the original screenshot and the edits on top of it.
//!
//! Edits reduce to state: which part of the original is visible and at what size it is output.
//! The canvas and the export are redrawn every time from the original and the state,
//! so undoing and restoring a crop never loses pixels.

use std::ptr;

use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};

use crate::annotation::{Annotation, Point};
use crate::backdrop::{self, Backdrop, Layout};
use crate::paint;
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGContext, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
    CGInterpolationQuality,
};

/// Rectangle in pixels, origin at the top left.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PixelRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl PixelRect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        PixelRect { x, y, width, height }
    }

    fn cg(self) -> CGRect {
        CGRect::new(CGPoint::new(self.x, self.y), CGSize::new(self.width, self.height))
    }
}

/// What is visible and at what size.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct State {
    /// Visible part of the original screenshot in its pixels.
    pub crop: PixelRect,
    /// Output size in pixels.
    pub width: f64,
    pub height: f64,
}

impl State {
    /// The whole screenshot at its own size.
    pub fn original(width: f64, height: f64) -> Self {
        State { crop: PixelRect::new(0.0, 0.0, width, height), width, height }
    }

    /// Crops to a rectangle in pixels of the current output.
    /// Edges are rounded to whole source pixels, otherwise the export would blur the border.
    pub fn cropped(self, rect: PixelRect) -> Self {
        let sx = self.crop.width / self.width;
        let sy = self.crop.height / self.height;
        let left = (self.crop.x + rect.x.max(0.0) * sx).round();
        let top = (self.crop.y + rect.y.max(0.0) * sy).round();
        let right = (self.crop.x + (rect.x + rect.width).min(self.width) * sx).round();
        let bottom = (self.crop.y + (rect.y + rect.height).min(self.height) * sy).round();
        let crop = PixelRect::new(left, top, (right - left).max(1.0), (bottom - top).max(1.0));
        State { crop, width: (crop.width / sx).round().max(1.0), height: (crop.height / sy).round().max(1.0) }
    }

    /// Restores everything cropped, keeping the current output pixel density.
    pub fn uncropped(self, original_width: f64, original_height: f64) -> Self {
        let density = self.width / self.crop.width;
        State {
            crop: PixelRect::new(0.0, 0.0, original_width, original_height),
            width: (original_width * density).round().max(1.0),
            height: (original_height * density).round().max(1.0),
        }
    }

    /// Changes the output width, the height follows the aspect ratio.
    pub fn resized(self, width: f64) -> Self {
        let width = width.round().max(1.0);
        State { width, height: (width * self.height / self.width).round().max(1.0), ..self }
    }

    pub fn is_cropped(self, original_width: f64, original_height: f64) -> bool {
        self.crop != PixelRect::new(0.0, 0.0, original_width, original_height)
    }

    /// Converts a point from output pixels to base pixels.
    pub fn to_source(self, x: f64, y: f64) -> Point {
        Point::new(self.crop.x + x * self.crop.width / self.width, self.crop.y + y * self.crop.height / self.height)
    }
}

/// The whole document: what is visible and at what size, which annotations and over which base.
/// The base changes only when annotations are flattened into the image.
#[derive(Clone, PartialEq, Debug)]
pub struct Doc {
    pub state: State,
    pub annotations: Vec<Annotation>,
    /// Base index in the editor base list. 0 is the original screenshot.
    pub base: usize,
    /// Backdrop under the screenshot. None means the screenshot as is.
    pub backdrop: Option<Backdrop>,
}

/// Undo and redo: a list of states and the position of the current one.
pub struct History<T> {
    states: Vec<T>,
    current: usize,
}

impl<T: Clone + PartialEq> History<T> {
    pub fn new(initial: T) -> Self {
        History { states: vec![initial], current: 0 }
    }

    pub fn current(&self) -> &T {
        &self.states[self.current]
    }

    /// New edit. Undone edits after the current state are dropped.
    pub fn push(&mut self, state: T) {
        if &state == self.current() {
            return;
        }
        self.states.truncate(self.current + 1);
        self.states.push(state);
        self.current += 1;
    }

    /// Replaces the current state without adding an undo step. This way a slider being dragged
    /// does not fill the history with intermediate values.
    pub fn amend(&mut self, state: T) {
        self.states.truncate(self.current + 1);
        self.states[self.current] = state;
    }

    pub fn can_undo(&self) -> bool {
        self.current > 0
    }

    pub fn can_redo(&self) -> bool {
        self.current + 1 < self.states.len()
    }

    pub fn undo(&mut self) -> bool {
        let moved = self.can_undo();
        if moved {
            self.current -= 1;
        }
        moved
    }

    pub fn redo(&mut self) -> bool {
        let moved = self.can_redo();
        if moved {
            self.current += 1;
        }
        moved
    }
}

/// Draws the output: the visible part of the base, annotations on top and then the target size.
/// Without annotations and resizing it is just a crop, without redrawing.
/// `images` are bases and added screenshots, `scale` is screenshot pixels per point, it sets line widths
/// and text size are converted to pixels. The canvas draws the `hidden` annotation on top while it is dragged.
pub fn render(
    images: &[CFRetained<CGImage>],
    doc: &Doc,
    hidden: Option<usize>,
    scale: f64,
) -> Option<CFRetained<CGImage>> {
    let (base, state) = (&*images[doc.base], doc.state);
    let cropped = CGImage::with_image_in_rect(Some(base), state.crop.cg())?;
    let annotated = if doc.annotations.is_empty() {
        cropped
    } else {
        let context = bitmap(base, state.crop.width as usize, state.crop.height as usize)?;
        let bounds = CGRect::new(CGPoint::ZERO, CGSize::new(state.crop.width, state.crop.height));
        CGContext::draw_image(Some(&context), bounds, Some(&cropped));
        paint::draw(&context, images, doc, hidden, scale);
        CGBitmapContextCreateImage(Some(&context))?
    };
    if state.width == state.crop.width && state.height == state.crop.height {
        return Some(annotated);
    }
    let context = bitmap(base, state.width as usize, state.height as usize)?;
    CGContext::set_interpolation_quality(Some(&context), CGInterpolationQuality::High);
    let bounds = CGRect::new(CGPoint::ZERO, CGSize::new(state.width, state.height));
    CGContext::draw_image(Some(&context), bounds, Some(&annotated));
    CGBitmapContextCreateImage(Some(&context))
}

/// Output with the backdrop, if any, and where the screenshot sits on it.
pub fn present(
    images: &[CFRetained<CGImage>],
    doc: &Doc,
    hidden: Option<usize>,
    scale: f64,
) -> Option<(CFRetained<CGImage>, Layout)> {
    let content = render(images, doc, hidden, scale)?;
    match &doc.backdrop {
        Some(backdrop) => backdrop::compose(&content, backdrop, scale),
        None => {
            let (width, height) = (doc.state.width, doc.state.height);
            Some((content, Layout { width, height, card: (0.0, 0.0, width, height), content: (0.0, 0.0) }))
        }
    }
}

/// Flattens annotations into the whole base, without cropping or resizing.
pub fn rasterize(images: &[CFRetained<CGImage>], doc: &Doc, scale: f64) -> Option<CFRetained<CGImage>> {
    let base = &images[doc.base];
    let (width, height) = (CGImage::width(Some(base)) as f64, CGImage::height(Some(base)) as f64);
    render(images, &Doc { state: State::original(width, height), ..doc.clone() }, None, scale)
}

/// 8-bit BGRA in the base color space.
fn bitmap(base: &CGImage, width: usize, height: usize) -> Option<CFRetained<CGContext>> {
    let space = CGImage::color_space(Some(base));
    // SAFETY: a null buffer makes Core Graphics allocate it.
    unsafe {
        CGBitmapContextCreate(
            ptr::null_mut(),
            width,
            height,
            8,
            0,
            space.as_deref(),
            CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0,
        )
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_graphics::CGColorSpace;

    use super::*;
    use crate::annotation::{Color, Size, Tool};
    use crate::output;

    /// Image where every pixel has its own color, so a one-pixel shift is visible.
    fn pattern(width: usize, height: usize) -> CFRetained<CGImage> {
        let mut pixels: Vec<u32> =
            (0..width * height).map(|i| 0xff00_0000 | ((i as u32).wrapping_mul(2_654_435_761) >> 8)).collect();
        let space = CGColorSpace::new_device_rgb();
        // SAFETY: the buffer of width * height 4-byte pixels lives until the end of the function, and the image copies it.
        let context = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                width,
                height,
                8,
                width * 4,
                space.as_deref(),
                CGImageAlphaInfo::NoneSkipFirst.0 | CGImageByteOrderInfo::Order32Little.0,
            )
        }
        .unwrap();
        CGBitmapContextCreateImage(Some(&context)).unwrap()
    }

    fn pixels(image: &CGImage) -> Vec<u32> {
        let (width, height) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
        let mut pixels = vec![0u32; width * height];
        let space = CGColorSpace::new_device_rgb();
        // SAFETY: the buffer of width * height 4-byte pixels outlives the context.
        let context = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                width,
                height,
                8,
                width * 4,
                space.as_deref(),
                CGImageAlphaInfo::NoneSkipFirst.0 | CGImageByteOrderInfo::Order32Little.0,
            )
        }
        .unwrap();
        let bounds = CGRect::new(CGPoint::ZERO, CGSize::new(width as f64, height as f64));
        CGContext::draw_image(Some(&context), bounds, Some(image));
        drop(context);
        pixels
    }

    fn doc(state: State, annotations: Vec<Annotation>) -> Doc {
        Doc { state, annotations, base: 0, backdrop: None }
    }

    #[test]
    fn exported_crop_matches_the_source_pixel_for_pixel() {
        let source = pattern(64, 40);
        let state = State::original(64.0, 40.0).cropped(PixelRect::new(5.0, 7.0, 20.0, 11.0));
        let rendered = render(std::slice::from_ref(&source), &doc(state, Vec::new()), None, 2.0).unwrap();
        let png = output::encode_png(&rendered, 2.0).unwrap();
        let (exported, scale) = output::decode(&png).unwrap();
        assert_eq!(scale, 2.0);

        let (source, exported) = (pixels(&source), pixels(&exported));
        assert_eq!(exported.len(), 20 * 11);
        for row in 0..11 {
            let from = (7 + row) * 64 + 5;
            assert_eq!(&exported[row * 20..row * 20 + 20], &source[from..from + 20], "row {row}");
        }
    }

    #[test]
    fn annotation_lands_on_its_source_pixels_inside_a_crop() {
        let source = pattern(64, 40);
        let state = State::original(64.0, 40.0).cropped(PixelRect::new(10.0, 10.0, 30.0, 20.0));
        // a 2-pixel line at source y = 15, which is rows 4 and 5 of the crop
        let mut line = Annotation::new(Tool::Line, Point::new(20.0, 15.0), Color::RED, Size::Small, 0);
        line.points[1] = Point::new(30.0, 15.0);
        let annotations = [line];
        // Color::RED in 8 bits
        let red = |p: u32| p & 0x00ff_ffff == 0x00ff_3b30;

        let images = [source.clone()];
        let drawn = pixels(&render(&images, &doc(state, annotations.to_vec()), None, 1.0).unwrap());
        let plain = pixels(&source);
        assert!(red(drawn[4 * 30 + 15]) && red(drawn[5 * 30 + 15]));
        // the rest is untouched
        assert_eq!(drawn[8 * 30 + 15], plain[18 * 64 + 25]);
        assert_eq!(drawn[0], plain[10 * 64 + 10]);

        let hidden = pixels(&render(&images, &doc(state, annotations.to_vec()), Some(0), 1.0).unwrap());
        assert_eq!(hidden[5 * 30 + 15], plain[15 * 64 + 25]);
    }

    #[test]
    fn crop_maps_through_an_earlier_resize_to_whole_source_pixels() {
        // 1000x600 screenshot, scaled down by half to 500x300
        let half = State::original(1000.0, 600.0).resized(500.0);
        assert_eq!((half.width, half.height), (500.0, 300.0));
        // a 100x50 crop at (10, 20) in the output is 200x100 at (20, 40) in the source
        let cropped = half.cropped(PixelRect::new(10.0, 20.0, 100.0, 50.0));
        assert_eq!(cropped.crop, PixelRect::new(20.0, 40.0, 200.0, 100.0));
        assert_eq!((cropped.width, cropped.height), (100.0, 50.0));
    }

    #[test]
    fn crop_outside_the_image_is_clamped_to_its_edges() {
        let state = State::original(400.0, 300.0).cropped(PixelRect::new(-50.0, 250.0, 200.0, 200.0));
        assert_eq!(state.crop, PixelRect::new(0.0, 250.0, 150.0, 50.0));
    }

    #[test]
    fn uncrop_restores_the_whole_image_at_the_current_density() {
        let state = State::original(1000.0, 600.0).resized(500.0).cropped(PixelRect::new(0.0, 0.0, 100.0, 100.0));
        let restored = state.uncropped(1000.0, 600.0);
        assert_eq!(restored.crop, PixelRect::new(0.0, 0.0, 1000.0, 600.0));
        assert_eq!((restored.width, restored.height), (500.0, 300.0));
        assert!(!restored.is_cropped(1000.0, 600.0));
    }

    #[test]
    fn history_undoes_redoes_and_drops_the_undone_branch() {
        let a = State::original(100.0, 100.0);
        let b = a.resized(50.0);
        let c = a.resized(80.0);
        let mut history = History::new(a);
        history.push(b);
        assert!(history.undo());
        assert_eq!(*history.current(), a);
        assert!(history.redo());
        assert_eq!(*history.current(), b);
        history.undo();
        // a new edit after undo forgets the undone one
        history.push(c);
        assert!(!history.can_redo());
        assert_eq!(*history.current(), c);
        // an edit without changes does not enter the history
        history.push(c);
        assert!(history.undo());
        assert_eq!(*history.current(), a);
        // an amend replaces the current state without a new step
        history.redo();
        history.amend(b);
        assert_eq!(*history.current(), b);
        assert!(history.undo());
        assert_eq!(*history.current(), a);
    }
}
