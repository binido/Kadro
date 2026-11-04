//! Документ редактора: исходный снимок и правки над ним.
//!
//! Правки сводятся к состоянию: какая часть исходного снимка видна и в каком размере она выводится.
//! Картинка на холсте и при экспорте каждый раз рисуется заново из исходного снимка и состояния,
//! поэтому отмена и возврат обрезанного не теряют пикселей.

use std::ptr;

use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGContext, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
    CGInterpolationQuality,
};

/// Прямоугольник в пикселях, начало вверху слева.
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

/// Что видно и в каком размере.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct State {
    /// Видимая часть исходного снимка в его пикселях.
    pub crop: PixelRect,
    /// Размер результата в пикселях.
    pub width: f64,
    pub height: f64,
}

impl State {
    /// Весь снимок в своём размере.
    pub fn original(width: f64, height: f64) -> Self {
        State { crop: PixelRect::new(0.0, 0.0, width, height), width, height }
    }

    /// Обрезает по прямоугольнику в пикселях текущего результата.
    /// Края округляются до целых пикселей исходника, иначе экспорт размазал бы границу.
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

    /// Возвращает всё обрезанное, сохраняя текущую плотность пикселей результата.
    pub fn uncropped(self, original_width: f64, original_height: f64) -> Self {
        let density = self.width / self.crop.width;
        State {
            crop: PixelRect::new(0.0, 0.0, original_width, original_height),
            width: (original_width * density).round().max(1.0),
            height: (original_height * density).round().max(1.0),
        }
    }

    /// Меняет ширину результата, высота следует пропорции.
    pub fn resized(self, width: f64) -> Self {
        let width = width.round().max(1.0);
        State { width, height: (width * self.height / self.width).round().max(1.0), ..self }
    }

    pub fn is_cropped(self, original_width: f64, original_height: f64) -> bool {
        self.crop != PixelRect::new(0.0, 0.0, original_width, original_height)
    }
}

/// Отмена и повтор: список состояний и место текущего в нём.
pub struct History {
    states: Vec<State>,
    current: usize,
}

impl History {
    pub fn new(initial: State) -> Self {
        History { states: vec![initial], current: 0 }
    }

    pub fn current(&self) -> State {
        self.states[self.current]
    }

    /// Новая правка. Отменённые после текущего состояния правки пропадают.
    pub fn push(&mut self, state: State) {
        if state == self.current() {
            return;
        }
        self.states.truncate(self.current + 1);
        self.states.push(state);
        self.current += 1;
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

/// Рисует результат из исходного снимка. Без изменения размера это просто вырезка, без перерисовки.
pub fn render(original: &CGImage, state: State) -> Option<CFRetained<CGImage>> {
    let cropped = CGImage::with_image_in_rect(Some(original), state.crop.cg())?;
    if state.width == state.crop.width && state.height == state.crop.height {
        return Some(cropped);
    }
    let (width, height) = (state.width as usize, state.height as usize);
    let space = CGImage::color_space(Some(original));
    // SAFETY: буфер null - Core Graphics выделит его сам. Параметры описывают 8-битный BGRA.
    let context = unsafe {
        CGBitmapContextCreate(
            ptr::null_mut(),
            width,
            height,
            8,
            0,
            space.as_deref(),
            CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0,
        )
    }?;
    CGContext::set_interpolation_quality(Some(&context), CGInterpolationQuality::High);
    let bounds = CGRect::new(CGPoint::ZERO, CGSize::new(state.width, state.height));
    CGContext::draw_image(Some(&context), bounds, Some(&cropped));
    CGBitmapContextCreateImage(Some(&context))
}

#[cfg(test)]
mod tests {
    use objc2_core_graphics::CGColorSpace;

    use super::*;
    use crate::output;

    /// Картинка, где у каждого пикселя свой цвет, чтобы сдвиг на пиксель был заметен.
    fn pattern(width: usize, height: usize) -> CFRetained<CGImage> {
        let mut pixels: Vec<u32> =
            (0..width * height).map(|i| 0xff00_0000 | ((i as u32).wrapping_mul(2_654_435_761) >> 8)).collect();
        let space = CGColorSpace::new_device_rgb();
        // SAFETY: буфер на width * height пикселей по 4 байта живёт до конца функции, а картинка копирует его.
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
        // SAFETY: буфер на width * height пикселей по 4 байта живёт дольше контекста.
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

    #[test]
    fn exported_crop_matches_the_source_pixel_for_pixel() {
        let source = pattern(64, 40);
        let state = State::original(64.0, 40.0).cropped(PixelRect::new(5.0, 7.0, 20.0, 11.0));
        let rendered = render(&source, state).unwrap();
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
    fn crop_maps_through_an_earlier_resize_to_whole_source_pixels() {
        // снимок 1000×600, уменьшен вдвое до 500×300
        let half = State::original(1000.0, 600.0).resized(500.0);
        assert_eq!((half.width, half.height), (500.0, 300.0));
        // обрезка 100×50 от точки (10, 20) в результате - это 200×100 от (20, 40) в исходнике
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
        assert_eq!(history.current(), a);
        assert!(history.redo());
        assert_eq!(history.current(), b);
        history.undo();
        // новая правка после отмены забывает отменённую
        history.push(c);
        assert!(!history.can_redo());
        assert_eq!(history.current(), c);
        // правка без изменений не попадает в историю
        history.push(c);
        assert!(history.undo());
        assert_eq!(history.current(), a);
    }
}
