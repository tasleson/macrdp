use anyhow::Result;
use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use foreign_types::ForeignType;

extern "C" {
    fn CGEventCreateScrollWheelEvent2(
        source: *mut core_graphics::sys::CGEventSource,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        wheel2: i32,
        wheel3: i32,
    ) -> *mut core_graphics::sys::CGEvent;
}

/// ScrollEventUnit::Line
const SCROLL_UNIT_LINE: u32 = 1;

/// RDP wheel rotation units per detent. MS-RDPBCGR defines one wheel "notch"
/// (`WHEEL_DELTA`) as 120 rotation units; clients send multiples of this.
const WHEEL_DELTA: i32 = 120;

/// macOS scroll lines emitted per wheel detent. The raw RDP value would request
/// ~120 lines per notch, which scrolls violently. One line per detent matches a
/// physical wheel mouse on a local Mac (one notch == one line of text). Tuned
/// for feel — adjust here rather than scattering magic numbers.
const LINES_PER_DETENT: i32 = 1;

/// Upper bound on lines emitted from a single scroll event, so a malformed or
/// unusually large rotation can't ask macOS to jump an absurd distance.
const MAX_SCROLL_LINES: i32 = 64;

pub struct MouseInjector;

impl MouseInjector {
    pub fn new() -> Result<Self> {
        let _ = CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|_| {
            anyhow::anyhow!("Failed to create CGEventSource — check Accessibility permission")
        })?;
        Ok(Self)
    }

    fn source() -> Result<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| anyhow::anyhow!("Failed to create CGEventSource"))
    }

    pub fn move_to(&self, x: u16, y: u16) -> Result<()> {
        let point = CGPoint::new(x as f64, y as f64);
        let source = Self::source()?;
        let event =
            CGEvent::new_mouse_event(source, CGEventType::MouseMoved, point, CGMouseButton::Left)
                .map_err(|_| anyhow::anyhow!("Failed to create mouse move event"))?;

        event.post(CGEventTapLocation::HID);
        tracing::trace!(x, y, "Mouse moved");
        Ok(())
    }

    pub fn button_event(&self, button: MouseButton, pressed: bool, x: u16, y: u16) -> Result<()> {
        let point = CGPoint::new(x as f64, y as f64);
        let (event_type, cg_button) = match (button, pressed) {
            (MouseButton::Left, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
            (MouseButton::Left, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
            (MouseButton::Right, true) => (CGEventType::RightMouseDown, CGMouseButton::Right),
            (MouseButton::Right, false) => (CGEventType::RightMouseUp, CGMouseButton::Right),
            (MouseButton::Middle, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
            (MouseButton::Middle, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
        };

        let source = Self::source()?;
        let event = CGEvent::new_mouse_event(source, event_type, point, cg_button)
            .map_err(|_| anyhow::anyhow!("Failed to create mouse button event"))?;

        event.post(CGEventTapLocation::HID);
        tracing::trace!(?button, pressed, x, y, "Mouse button event");
        Ok(())
    }

    /// Inject a scroll event. `dx` and `dy` are in RDP wheel rotation units
    /// ([`WHEEL_DELTA`] per detent); positive `dy` scrolls up and positive `dx`
    /// scrolls right, matching the RDP wire convention.
    pub fn scroll(&self, dx: i32, dy: i32) -> Result<()> {
        let vertical = Self::units_to_lines(dy);
        let horizontal = Self::units_to_lines(dx);

        // Nothing to do — avoids emitting empty scroll events for sub-detent jitter.
        if vertical == 0 && horizontal == 0 {
            return Ok(());
        }

        unsafe {
            let event_ref = CGEventCreateScrollWheelEvent2(
                std::ptr::null_mut(),
                SCROLL_UNIT_LINE,
                2,
                vertical,
                horizontal,
                0,
            );
            if event_ref.is_null() {
                return Err(anyhow::anyhow!("Failed to create scroll event"));
            }
            let event = CGEvent::from_ptr(event_ref);
            event.post(CGEventTapLocation::HID);
        }
        tracing::trace!(horizontal, vertical, "Mouse scroll");
        Ok(())
    }

    /// Convert RDP wheel rotation units into macOS scroll lines, preserving the
    /// sign and clamping the magnitude. Multiplying before dividing means that if
    /// `LINES_PER_DETENT` is ever raised above 1, sub-detent rotations still scale
    /// proportionally rather than all truncating to zero. At one line per detent,
    /// rotations smaller than a full notch round down to no movement.
    fn units_to_lines(units: i32) -> i32 {
        let lines = units.saturating_mul(LINES_PER_DETENT) / WHEEL_DELTA;
        lines.clamp(-MAX_SCROLL_LINES, MAX_SCROLL_LINES)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_to_lines_maps_detents() {
        // One detent (WHEEL_DELTA units) yields LINES_PER_DETENT lines, sign preserved.
        assert_eq!(MouseInjector::units_to_lines(WHEEL_DELTA), LINES_PER_DETENT);
        assert_eq!(
            MouseInjector::units_to_lines(-WHEEL_DELTA),
            -LINES_PER_DETENT
        );
        assert_eq!(
            MouseInjector::units_to_lines(2 * WHEEL_DELTA),
            2 * LINES_PER_DETENT
        );
    }

    #[test]
    fn units_to_lines_zero_is_zero() {
        assert_eq!(MouseInjector::units_to_lines(0), 0);
    }

    #[test]
    fn units_to_lines_truncates_sub_detent_rotations() {
        // At one line per detent, a rotation smaller than a full notch produces
        // no movement (rounds toward zero), preserving sign semantics.
        assert_eq!(MouseInjector::units_to_lines(WHEEL_DELTA / 2), 0);
        assert_eq!(MouseInjector::units_to_lines(-WHEEL_DELTA / 2), 0);
    }

    #[test]
    fn units_to_lines_clamps_large_bursts() {
        assert_eq!(MouseInjector::units_to_lines(i32::MAX), MAX_SCROLL_LINES);
        assert_eq!(MouseInjector::units_to_lines(i32::MIN), -MAX_SCROLL_LINES);
    }
}
