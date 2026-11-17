//! Rectangle geometry shared by CG and Cocoa coordinates.

use objc2_core_foundation::{CGPoint, CGRect};

/// Whether the point is inside the rectangle. The right and bottom edges are excluded
/// so a point on the seam between two displays belongs to only one of them.
pub fn contains(rect: CGRect, point: CGPoint) -> bool {
    point.x >= rect.origin.x
        && point.x < rect.origin.x + rect.size.width
        && point.y >= rect.origin.y
        && point.y < rect.origin.y + rect.size.height
}

/// Rectangle center.
pub fn center(rect: CGRect) -> CGPoint {
    CGPoint::new(rect.origin.x + rect.size.width / 2.0, rect.origin.y + rect.size.height / 2.0)
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;

    #[test]
    fn point_on_the_far_edge_belongs_to_the_next_rect() {
        let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(100.0, 50.0));
        assert!(contains(rect, CGPoint::new(0.0, 0.0)));
        assert!(contains(rect, CGPoint::new(99.5, 49.5)));
        assert!(!contains(rect, CGPoint::new(100.0, 10.0)));
        assert!(!contains(rect, CGPoint::new(10.0, 50.0)));
    }

    #[test]
    fn center_is_in_the_middle_of_both_sides() {
        let rect = CGRect::new(CGPoint::new(-20.0, 10.0), CGSize::new(40.0, 30.0));
        assert_eq!(center(rect), CGPoint::new(0.0, 25.0));
    }
}
