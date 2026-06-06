use ironrdp_ainput as ainput;
use ironrdp_pdu::input::fast_path::{self, SynchronizeFlags};
use ironrdp_pdu::input::mouse::PointerFlags;
use ironrdp_pdu::input::mouse_rel::PointerRelFlags;
use ironrdp_pdu::input::mouse_x::PointerXFlags;
use ironrdp_pdu::input::sync::SyncToggleFlags;
use ironrdp_pdu::input::{scan_code, unicode, MousePdu, MouseRelPdu, MouseXPdu};

/// Keyboard Event
///
/// Describes a keyboard event received from the client
///
#[derive(Debug)]
pub enum KeyboardEvent {
    Pressed { code: u8, extended: bool },
    Released { code: u8, extended: bool },
    UnicodePressed(u16),
    UnicodeReleased(u16),
    Synchronize(SynchronizeFlags),
}

/// Mouse Event
///
/// Describes a mouse event received from the client
///
#[derive(Debug)]
pub enum MouseEvent {
    Move {
        x: u16,
        y: u16,
    },
    RightPressed,
    RightReleased,
    LeftPressed,
    LeftReleased,
    MiddlePressed,
    MiddleReleased,
    Button4Pressed,
    Button4Released,
    Button5Pressed,
    Button5Released,
    /// Wheel scroll in RDP rotation units (120 per detent); `x` is horizontal,
    /// `y` is vertical, both following the RDP sign convention (positive = up/right).
    Scroll {
        x: i32,
        y: i32,
    },
    RelMove {
        x: i32,
        y: i32,
    },
}

/// Input Event Handler for an RDP server
///
/// Whenever the RDP server will receive an input event from a client, the relevant callback from
/// this handler will be called
///
/// # Example
///
/// ```
/// use ironrdp_server::{KeyboardEvent, MouseEvent, RdpServerInputHandler};
///
/// pub struct InputHandler;
///
/// impl RdpServerInputHandler for InputHandler {
///     fn keyboard(&mut self, event: KeyboardEvent) {
///         match event {
///             KeyboardEvent::Pressed { code, .. } => println!("Pressed {}", code),
///             KeyboardEvent::Released { code, .. } => println!("Released {}", code),
///             other => println!("unhandled event: {:?}", other),
///         };
///     }
///
///     fn mouse(&mut self, event: MouseEvent) {
///         let result = match event {
///             MouseEvent::Move { x, y } => println!("Moved mouse to {} {}", x, y),
///             other => println!("unhandled event: {:?}", other),
///         };
///     }
/// }
/// ```
pub trait RdpServerInputHandler: Send {
    fn keyboard(&mut self, event: KeyboardEvent);
    fn mouse(&mut self, event: MouseEvent);

    /// Release any input state the handler may have injected into the host.
    ///
    /// Called when a client session ends so a dropped release event cannot leave
    /// the host with a stuck remote mouse button or modifier key.
    fn reset(&mut self) {}
}

impl From<(u8, fast_path::KeyboardFlags)> for KeyboardEvent {
    fn from((key, flags): (u8, fast_path::KeyboardFlags)) -> Self {
        let extended = flags.contains(fast_path::KeyboardFlags::EXTENDED);
        if flags.contains(fast_path::KeyboardFlags::RELEASE) {
            KeyboardEvent::Released {
                code: key,
                extended,
            }
        } else {
            KeyboardEvent::Pressed {
                code: key,
                extended,
            }
        }
    }
}

impl From<(u16, fast_path::KeyboardFlags)> for KeyboardEvent {
    fn from((key, flags): (u16, fast_path::KeyboardFlags)) -> Self {
        if flags.contains(fast_path::KeyboardFlags::RELEASE) {
            KeyboardEvent::UnicodeReleased(key)
        } else {
            KeyboardEvent::UnicodePressed(key)
        }
    }
}

impl From<(u16, scan_code::KeyboardFlags)> for KeyboardEvent {
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "we are truncating the value on purpose"
    )]
    fn from((key, flags): (u16, scan_code::KeyboardFlags)) -> Self {
        let extended = flags.contains(scan_code::KeyboardFlags::EXTENDED);

        if flags.contains(scan_code::KeyboardFlags::RELEASE) {
            KeyboardEvent::Released {
                code: key as u8,
                extended,
            }
        } else {
            KeyboardEvent::Pressed {
                code: key as u8,
                extended,
            }
        }
    }
}

impl From<(u16, unicode::KeyboardFlags)> for KeyboardEvent {
    fn from((key, flags): (u16, unicode::KeyboardFlags)) -> Self {
        if flags.contains(unicode::KeyboardFlags::RELEASE) {
            KeyboardEvent::UnicodeReleased(key)
        } else {
            KeyboardEvent::UnicodePressed(key)
        }
    }
}

impl From<SynchronizeFlags> for KeyboardEvent {
    fn from(value: SynchronizeFlags) -> Self {
        KeyboardEvent::Synchronize(value)
    }
}

impl From<SyncToggleFlags> for KeyboardEvent {
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "we are truncating the value on purpose"
    )]
    fn from(value: SyncToggleFlags) -> Self {
        KeyboardEvent::Synchronize(SynchronizeFlags::from_bits_truncate(value.bits() as u8))
    }
}

impl From<MousePdu> for MouseEvent {
    fn from(value: MousePdu) -> Self {
        if value.flags.contains(PointerFlags::LEFT_BUTTON) {
            if value.flags.contains(PointerFlags::DOWN) {
                MouseEvent::LeftPressed
            } else {
                MouseEvent::LeftReleased
            }
        } else if value.flags.contains(PointerFlags::RIGHT_BUTTON) {
            if value.flags.contains(PointerFlags::DOWN) {
                MouseEvent::RightPressed
            } else {
                MouseEvent::RightReleased
            }
        } else if value.flags.contains(PointerFlags::VERTICAL_WHEEL) {
            MouseEvent::Scroll {
                x: 0,
                y: value.number_of_wheel_rotation_units.into(),
            }
        } else if value.flags.contains(PointerFlags::HORIZONTAL_WHEEL) {
            // A horizontal-wheel PDU carries the rotation in the wheel field, not
            // a cursor position; without this branch it would fall through to
            // `Move` and teleport the pointer.
            MouseEvent::Scroll {
                x: value.number_of_wheel_rotation_units.into(),
                y: 0,
            }
        } else {
            MouseEvent::Move {
                x: value.x_position,
                y: value.y_position,
            }
        }
    }
}

impl From<MouseXPdu> for MouseEvent {
    fn from(value: MouseXPdu) -> Self {
        if value.flags.contains(PointerXFlags::BUTTON1) {
            if value.flags.contains(PointerXFlags::DOWN) {
                MouseEvent::LeftPressed
            } else {
                MouseEvent::LeftReleased
            }
        } else if value.flags.contains(PointerXFlags::BUTTON2) {
            if value.flags.contains(PointerXFlags::DOWN) {
                MouseEvent::RightPressed
            } else {
                MouseEvent::RightReleased
            }
        } else {
            MouseEvent::Move {
                x: value.x_position,
                y: value.y_position,
            }
        }
    }
}

impl From<MouseRelPdu> for MouseEvent {
    fn from(value: MouseRelPdu) -> Self {
        if value.flags.contains(PointerRelFlags::BUTTON1) {
            if value.flags.contains(PointerRelFlags::DOWN) {
                MouseEvent::LeftPressed
            } else {
                MouseEvent::LeftReleased
            }
        } else if value.flags.contains(PointerRelFlags::BUTTON2) {
            if value.flags.contains(PointerRelFlags::DOWN) {
                MouseEvent::RightPressed
            } else {
                MouseEvent::RightReleased
            }
        } else if value.flags.contains(PointerRelFlags::BUTTON3) {
            if value.flags.contains(PointerRelFlags::DOWN) {
                MouseEvent::MiddlePressed
            } else {
                MouseEvent::MiddleReleased
            }
        } else if value.flags.contains(PointerRelFlags::XBUTTON1) {
            if value.flags.contains(PointerRelFlags::DOWN) {
                MouseEvent::Button4Pressed
            } else {
                MouseEvent::Button4Released
            }
        } else if value.flags.contains(PointerRelFlags::XBUTTON2) {
            if value.flags.contains(PointerRelFlags::DOWN) {
                MouseEvent::Button5Pressed
            } else {
                MouseEvent::Button5Released
            }
        } else {
            MouseEvent::RelMove {
                x: value.x_delta.into(),
                y: value.y_delta.into(),
            }
        }
    }
}

/// Fixed-point scale FreeRDP applies to ainput wheel rotation. Its client
/// multiplies the legacy WHEEL_DELTA value by `0x10000` before sending it over
/// the advanced-input channel, so the wire value is WHEEL_DELTA units << 16.
const AINPUT_WHEEL_SCALE: i32 = 0x10000;

impl From<ainput::MousePdu> for MouseEvent {
    fn from(value: ainput::MousePdu) -> Self {
        use ainput::MouseEventFlags;

        if value.flags.contains(MouseEventFlags::BUTTON1) {
            if value.flags.contains(MouseEventFlags::DOWN) {
                MouseEvent::LeftPressed
            } else {
                MouseEvent::LeftReleased
            }
        } else if value.flags.contains(MouseEventFlags::BUTTON2) {
            if value.flags.contains(MouseEventFlags::DOWN) {
                MouseEvent::RightPressed
            } else {
                MouseEvent::RightReleased
            }
        } else if value.flags.contains(MouseEventFlags::BUTTON3) {
            if value.flags.contains(MouseEventFlags::DOWN) {
                MouseEvent::MiddlePressed
            } else {
                MouseEvent::MiddleReleased
            }
        } else if value.flags.contains(MouseEventFlags::WHEEL) {
            // FreeRDP's freerdp_client_send_wheel_event scales the WHEEL_DELTA
            // rotation by 0x10000 before putting it on the ainput channel
            // (`value *= 0x10000`), so a single detent arrives as 120 * 65536.
            // Normalize back to plain WHEEL_DELTA units so Scroll carries the
            // same units as the legacy MousePdu path.
            MouseEvent::Scroll {
                x: value.x / AINPUT_WHEEL_SCALE,
                y: value.y / AINPUT_WHEEL_SCALE,
            }
        } else if value.flags.contains(MouseEventFlags::REL) {
            MouseEvent::RelMove {
                x: value.x,
                y: value.y,
            }
        } else if value.flags.contains(MouseEventFlags::MOVE) {
            // assume moves are 0 <= u16::MAX
            MouseEvent::Move {
                x: value.x.try_into().unwrap_or(0),
                y: value.y.try_into().unwrap_or(0),
            }
        } else {
            MouseEvent::Move { x: 0, y: 0 }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mouse_pdu(flags: PointerFlags, wheel: i16) -> MousePdu {
        MousePdu {
            flags,
            number_of_wheel_rotation_units: wheel,
            // Non-zero coordinates so a misrouted wheel event would surface as a
            // bogus Move to these positions rather than a quiet zero.
            x_position: 100,
            y_position: 200,
        }
    }

    #[test]
    fn vertical_wheel_maps_to_vertical_scroll() {
        let event = MouseEvent::from(mouse_pdu(PointerFlags::VERTICAL_WHEEL, 120));
        assert!(
            matches!(event, MouseEvent::Scroll { x: 0, y: 120 }),
            "got {event:?}"
        );
    }

    #[test]
    fn horizontal_wheel_maps_to_horizontal_scroll_not_move() {
        // Regression: HORIZONTAL_WHEEL used to fall through to Move and teleport
        // the pointer to the wheel-rotation value held in the position fields.
        let event = MouseEvent::from(mouse_pdu(PointerFlags::HORIZONTAL_WHEEL, -120));
        assert!(
            matches!(event, MouseEvent::Scroll { x: -120, y: 0 }),
            "got {event:?}"
        );
    }

    #[test]
    fn plain_move_still_carries_position() {
        let event = MouseEvent::from(mouse_pdu(PointerFlags::MOVE, 0));
        assert!(
            matches!(event, MouseEvent::Move { x: 100, y: 200 }),
            "got {event:?}"
        );
    }

    fn ainput_wheel_pdu(x: i32, y: i32) -> ainput::MousePdu {
        ainput::MousePdu {
            time: 0,
            flags: ainput::MouseEventFlags::WHEEL,
            x,
            y,
        }
    }

    #[test]
    fn ainput_wheel_normalizes_fixed_point_to_wheel_delta_units() {
        // FreeRDP sends one detent as 120 << 16 over ainput; it must arrive as
        // plain WHEEL_DELTA units (120) so it matches the legacy MousePdu path.
        let one_detent = 120 * AINPUT_WHEEL_SCALE;
        let event = MouseEvent::from(ainput_wheel_pdu(0, one_detent));
        assert!(
            matches!(event, MouseEvent::Scroll { x: 0, y: 120 }),
            "got {event:?}"
        );

        let event = MouseEvent::from(ainput_wheel_pdu(-one_detent, 0));
        assert!(
            matches!(event, MouseEvent::Scroll { x: -120, y: 0 }),
            "got {event:?}"
        );
    }
}
