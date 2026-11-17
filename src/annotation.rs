//! Annotations over a screenshot: what is drawn, where and in which style.
//!
//! Coordinates are in pixels of the document base with the origin at the top left. So cropping
//! and resizing do not move annotations, they only change which part of them is visible.

/// Point in base pixels, origin at the top left.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Point { x, y }
    }

    fn distance(self, other: Point) -> f64 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Arrow,
    Line,
    Rect,
    Oval,
    Pen,
    Highlighter,
    Text,
    Counter,
    Blur,
    Pixelate,
    Spotlight,
    Magnifier,
    Ruler,
    /// Screenshot added on top of the base.
    Image,
}

impl Tool {
    pub const ALL: [Tool; 14] = [
        Tool::Arrow,
        Tool::Line,
        Tool::Rect,
        Tool::Oval,
        Tool::Pen,
        Tool::Highlighter,
        Tool::Text,
        Tool::Counter,
        Tool::Blur,
        Tool::Pixelate,
        Tool::Spotlight,
        Tool::Magnifier,
        Tool::Ruler,
        Tool::Image,
    ];

    pub fn index(self) -> usize {
        Tool::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    /// Tools defined by two corners or two ends.
    pub fn is_two_point(self) -> bool {
        !matches!(self, Tool::Pen | Tool::Highlighter | Tool::Text | Tool::Counter)
    }

    /// Style variants for the popup menu. The variant index is stored in `Annotation::style`.
    pub fn styles(self) -> &'static [&'static str] {
        match self {
            Tool::Arrow => &["Filled head", "Open head", "Both ends"],
            Tool::Line => &["Solid", "Dashed"],
            Tool::Rect => &["Outline", "Rounded", "Filled"],
            Tool::Oval => &["Outline", "Filled"],
            Tool::Text => &["Plain", "With background"],
            Tool::Blur | Tool::Pixelate => &["Light", "Medium", "Strong"],
            Tool::Spotlight => &["Rectangle", "Ellipse"],
            Tool::Magnifier => &["2×", "3×", "4×"],
            Tool::Ruler => &["Pixels", "Points"],
            Tool::Pen | Tool::Highlighter | Tool::Counter | Tool::Image => &[],
        }
    }
}

/// Size of strokes, text and counters. Points are multiplied by the screenshot scale so annotations
/// look the same on Retina and regular displays.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Size {
    Small,
    Medium,
    Large,
    Huge,
}

impl Size {
    pub const ALL: [Size; 4] = [Size::Small, Size::Medium, Size::Large, Size::Huge];

    pub fn title(self) -> &'static str {
        match self {
            Size::Small => "S",
            Size::Medium => "M",
            Size::Large => "L",
            Size::Huge => "XL",
        }
    }

    /// Stroke width in points.
    pub fn stroke(self) -> f64 {
        match self {
            Size::Small => 2.0,
            Size::Medium => 4.0,
            Size::Large => 6.0,
            Size::Huge => 10.0,
        }
    }

    /// Text size in points.
    pub fn text(self) -> f64 {
        match self {
            Size::Small => 14.0,
            Size::Medium => 20.0,
            Size::Large => 28.0,
            Size::Huge => 40.0,
        }
    }

    /// Counter diameter in points.
    pub fn counter(self) -> f64 {
        match self {
            Size::Small => 22.0,
            Size::Medium => 30.0,
            Size::Large => 40.0,
            Size::Huge => 52.0,
        }
    }
}

/// sRGB color, components from 0 to 1.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Color {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
}

impl Color {
    pub const RED: Color = Color { red: 1.0, green: 0.231, blue: 0.188, alpha: 1.0 };
}

#[derive(Clone, PartialEq, Debug)]
pub struct Annotation {
    pub tool: Tool,
    /// Two ends or corners, stroke points, or a single anchor point for text and counters.
    pub points: Vec<Point>,
    pub color: Color,
    pub size: Size,
    pub style: usize,
    pub text: String,
    /// Width and height of the text or counter diameter in pixels. Rendering computes them,
    /// they are kept here for mouse hit testing on text and counters.
    pub extent: (f64, f64),
    /// Index in the editor image list, for an added screenshot.
    pub image: usize,
}

/// What the mouse grabbed: the whole annotation or one of its points.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Grip {
    Body,
    Point(usize),
}

impl Annotation {
    pub fn new(tool: Tool, at: Point, color: Color, size: Size, style: usize) -> Self {
        let points = if tool.is_two_point() { vec![at, at] } else { vec![at] };
        Annotation { tool, points, color, size, style, text: String::new(), extent: (0.0, 0.0), image: 0 }
    }

    /// Rectangle from two corners. For other tools, the bounds of all points.
    pub fn rect(&self) -> (Point, Point) {
        let xs = self.points.iter().map(|p| p.x);
        let ys = self.points.iter().map(|p| p.y);
        let min = Point::new(xs.clone().fold(f64::INFINITY, f64::min), ys.clone().fold(f64::INFINITY, f64::min));
        let max = Point::new(xs.fold(f64::NEG_INFINITY, f64::max), ys.fold(f64::NEG_INFINITY, f64::max));
        (min, max)
    }

    /// A shape this small is an accidental click, not a drawing.
    pub fn is_degenerate(&self) -> bool {
        match self.tool {
            Tool::Text | Tool::Counter => false,
            Tool::Pen | Tool::Highlighter => self.points.len() < 2,
            _ => self.points[0].distance(self.points[1]) < 3.0,
        }
    }

    /// What the point hits, within `tolerance` pixels. Ends and corners are checked before the body
    /// so they can be dragged.
    pub fn hit(&self, p: Point, tolerance: f64) -> Option<Grip> {
        if self.tool.is_two_point()
            && let Some(index) = self.points.iter().position(|q| q.distance(p) <= tolerance)
        {
            return Some(Grip::Point(index));
        }
        let inside = match self.tool {
            Tool::Arrow | Tool::Line | Tool::Ruler => {
                distance_to_segment(p, self.points[0], self.points[1]) <= tolerance
            }
            Tool::Pen | Tool::Highlighter => {
                self.points.windows(2).any(|w| distance_to_segment(p, w[0], w[1]) <= tolerance)
            }
            Tool::Text | Tool::Counter => {
                let (min, max) = self.extent_rect();
                p.x >= min.x - tolerance
                    && p.x <= max.x + tolerance
                    && p.y >= min.y - tolerance
                    && p.y <= max.y + tolerance
            }
            _ => {
                let (min, max) = self.rect();
                p.x >= min.x - tolerance
                    && p.x <= max.x + tolerance
                    && p.y >= min.y - tolerance
                    && p.y <= max.y + tolerance
            }
        };
        inside.then_some(Grip::Body)
    }

    /// Area taken by text (from the top left corner) or a counter (around its center).
    pub fn extent_rect(&self) -> (Point, Point) {
        let p = self.points[0];
        let (width, height) = self.extent;
        match self.tool {
            Tool::Counter => {
                (Point::new(p.x - width / 2.0, p.y - height / 2.0), Point::new(p.x + width / 2.0, p.y + height / 2.0))
            }
            _ => (p, Point::new(p.x + width, p.y + height)),
        }
    }

    /// Occupied rectangle: bounds of the points or the area of text and counters.
    pub fn bounds(&self) -> (Point, Point) {
        match self.tool {
            Tool::Text | Tool::Counter => self.extent_rect(),
            _ => self.rect(),
        }
    }

    /// Moves the whole annotation.
    pub fn moved(&self, dx: f64, dy: f64) -> Self {
        let points = self.points.iter().map(|p| Point::new(p.x + dx, p.y + dy)).collect();
        Annotation { points, ..self.clone() }
    }

    /// Moves one point, a line end or a rectangle corner.
    pub fn with_point(&self, index: usize, p: Point) -> Self {
        let mut points = self.points.clone();
        if let Some(point) = points.get_mut(index) {
            *point = p;
        }
        Annotation { points, ..self.clone() }
    }
}

/// Topmost annotation under the point. The most recently drawn ones are on top.
pub fn hit_test(annotations: &[Annotation], p: Point, tolerance: f64) -> Option<(usize, Grip)> {
    annotations.iter().enumerate().rev().find_map(|(index, a)| a.hit(p, tolerance).map(|grip| (index, grip)))
}

/// Snap result: offset correction and the lines it snapped to, vertical x and horizontal y.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Snap {
    pub dx: f64,
    pub dy: f64,
    pub x: Option<f64>,
    pub y: Option<f64>,
}

/// Pulls the edges or center of `moving` to the nearest edges or centers of `targets`
/// when they are closer than `tolerance`. Each axis snaps on its own.
pub fn snap(moving: (Point, Point), targets: &[(Point, Point)], tolerance: f64) -> Snap {
    let lines = |min: f64, max: f64| [min, (min + max) / 2.0, max];
    let axis = |own: [f64; 3], others: &mut dyn Iterator<Item = [f64; 3]>| {
        others
            .flat_map(IntoIterator::into_iter)
            .flat_map(|target| own.map(|line| (target - line, target)))
            .filter(|(shift, _)| shift.abs() <= tolerance)
            .min_by(|a, b| a.0.abs().total_cmp(&b.0.abs()))
    };
    let (min, max) = moving;
    let x = axis(lines(min.x, max.x), &mut targets.iter().map(|(a, b)| lines(a.x, b.x)));
    let y = axis(lines(min.y, max.y), &mut targets.iter().map(|(a, b)| lines(a.y, b.y)));
    Snap { dx: x.map_or(0.0, |s| s.0), dy: y.map_or(0.0, |s| s.0), x: x.map(|s| s.1), y: y.map(|s| s.1) }
}

/// Counter number is its position among counters. Deleting one renumbers the rest.
pub fn counter_number(annotations: &[Annotation], index: usize) -> usize {
    annotations[..=index].iter().filter(|a| a.tool == Tool::Counter).count()
}

fn distance_to_segment(p: Point, a: Point, b: Point) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length = dx * dx + dy * dy;
    if length == 0.0 {
        return p.distance(a);
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / length).clamp(0.0, 1.0);
    p.distance(Point::new(a.x + t * dx, a.y + t * dy))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(from: (f64, f64), to: (f64, f64)) -> Annotation {
        let mut a = Annotation::new(Tool::Line, Point::new(from.0, from.1), Color::RED, Size::Medium, 0);
        a.points[1] = Point::new(to.0, to.1);
        a
    }

    #[test]
    fn line_is_hit_along_its_length_and_grabbed_by_its_ends() {
        let a = line((0.0, 0.0), (100.0, 0.0));
        assert_eq!(a.hit(Point::new(50.0, 3.0), 5.0), Some(Grip::Body));
        assert_eq!(a.hit(Point::new(50.0, 9.0), 5.0), None);
        // outside the segment
        assert_eq!(a.hit(Point::new(110.0, 0.0), 5.0), None);
        assert_eq!(a.hit(Point::new(99.0, 1.0), 5.0), Some(Grip::Point(1)));
    }

    #[test]
    fn topmost_annotation_wins_and_moves_as_a_whole() {
        let mut rect = Annotation::new(Tool::Rect, Point::new(10.0, 10.0), Color::RED, Size::Medium, 0);
        rect.points[1] = Point::new(60.0, 40.0);
        let annotations = vec![rect.clone(), line((0.0, 20.0), (100.0, 20.0))];
        assert_eq!(hit_test(&annotations, Point::new(30.0, 20.0), 4.0), Some((1, Grip::Body)));
        assert_eq!(hit_test(&annotations, Point::new(30.0, 35.0), 4.0), Some((0, Grip::Body)));
        let moved = rect.moved(5.0, -2.0);
        assert_eq!(moved.points, vec![Point::new(15.0, 8.0), Point::new(65.0, 38.0)]);
        assert_eq!(rect.with_point(1, Point::new(80.0, 50.0)).rect(), (Point::new(10.0, 10.0), Point::new(80.0, 50.0)));
    }

    #[test]
    fn counters_renumber_when_one_is_removed() {
        let counter = |x| Annotation::new(Tool::Counter, Point::new(x, 0.0), Color::RED, Size::Medium, 0);
        let mut text = Annotation::new(Tool::Text, Point::new(100.0, 100.0), Color::RED, Size::Medium, 0);
        text.extent = (80.0, 20.0);
        assert_eq!(text.hit(Point::new(170.0, 115.0), 0.0), Some(Grip::Body));
        assert_eq!(text.hit(Point::new(190.0, 115.0), 0.0), None);
        let mut annotations = vec![counter(0.0), line((0.0, 0.0), (1.0, 1.0)), counter(10.0), counter(20.0)];
        assert_eq!(counter_number(&annotations, 3), 3);
        annotations.remove(0);
        assert_eq!(counter_number(&annotations, 1), 1);
        assert_eq!(counter_number(&annotations, 2), 2);
    }

    #[test]
    fn moved_box_snaps_to_the_nearest_edge_or_centre_on_each_axis() {
        let target = (Point::new(100.0, 100.0), Point::new(200.0, 160.0));
        // left edge 203 snaps to the target right edge 200, top edge 127 to its center 130
        let moving = (Point::new(203.0, 127.0), Point::new(243.0, 147.0));
        let snapped = snap(moving, &[target], 5.0);
        assert_eq!((snapped.dx, snapped.x), (-3.0, Some(200.0)));
        assert_eq!((snapped.dy, snapped.y), (3.0, Some(130.0)));
        // beyond tolerance, no correction
        let far = (Point::new(500.0, 500.0), Point::new(520.0, 520.0));
        assert_eq!(snap(far, &[target], 5.0), Snap::default());
    }

    #[test]
    fn a_click_without_drag_is_not_a_shape() {
        let click = Annotation::new(Tool::Arrow, Point::new(5.0, 5.0), Color::RED, Size::Medium, 0);
        assert!(click.is_degenerate());
        assert!(!line((0.0, 0.0), (10.0, 0.0)).is_degenerate());
        assert!(!Annotation::new(Tool::Counter, Point::new(5.0, 5.0), Color::RED, Size::Medium, 0).is_degenerate());
    }
}
