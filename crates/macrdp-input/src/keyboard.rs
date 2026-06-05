use anyhow::Result;
use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use std::collections::BTreeSet;

use crate::keymap::scancode_to_keycode;

pub struct KeyboardInjector {
    modifier_flags: CGEventFlags,
    pressed_modifiers: BTreeSet<u16>,
}

impl KeyboardInjector {
    pub fn new() -> Result<Self> {
        // Verify we can create an event source (permission check)
        let _ = CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|_| {
            anyhow::anyhow!("Failed to create CGEventSource — check Accessibility permission")
        })?;
        Ok(Self {
            modifier_flags: CGEventFlags::CGEventFlagNull,
            pressed_modifiers: BTreeSet::new(),
        })
    }

    fn source() -> Result<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| anyhow::anyhow!("Failed to create CGEventSource"))
    }

    /// Inject a key press or release event
    pub fn inject_key(&mut self, scancode: u8, extended: bool, pressed: bool) -> Result<()> {
        let keycode = match scancode_to_keycode(scancode, extended) {
            Some(kc) => kc,
            None => {
                tracing::warn!(scancode, extended, "Unknown scancode, ignoring");
                return Ok(());
            }
        };

        let source = Self::source()?;
        let event = CGEvent::new_keyboard_event(source, keycode as CGKeyCode, pressed)
            .map_err(|_| anyhow::anyhow!("Failed to create keyboard event"))?;

        let flags = self.flags_for_event(keycode, pressed);
        event.set_flags(flags);
        event.post(CGEventTapLocation::HID);
        tracing::trace!(scancode, keycode, pressed, "Keyboard event injected");
        Ok(())
    }

    /// Inject a unicode character press/release
    pub fn inject_unicode(&mut self, ch: u16, pressed: bool) -> Result<()> {
        if !pressed {
            return Ok(());
        }

        let source = Self::source()?;
        let event = CGEvent::new_keyboard_event(source, 0, pressed)
            .map_err(|_| anyhow::anyhow!("Failed to create unicode event"))?;

        event.set_flags(CGEventFlags::CGEventFlagNull);
        event.set_string_from_utf16_unchecked(&[ch]);

        event.post(CGEventTapLocation::HID);
        Ok(())
    }

    /// Release any modifiers macrdp believes it has pressed.
    pub fn reset_modifiers(&mut self) -> Result<()> {
        let modifiers: Vec<u16> = self.pressed_modifiers.iter().copied().collect();
        for keycode in modifiers {
            if let Some(flag) = modifier_flag_for_keycode(keycode) {
                self.modifier_flags.remove(flag);
                let source = Self::source()?;
                let event = CGEvent::new_keyboard_event(source, keycode as CGKeyCode, false)
                    .map_err(|_| anyhow::anyhow!("Failed to create modifier release event"))?;
                event.set_flags(self.modifier_flags);
                event.post(CGEventTapLocation::HID);
            }
        }
        self.pressed_modifiers.clear();
        self.modifier_flags = CGEventFlags::CGEventFlagNull;
        Ok(())
    }

    fn flags_for_event(&mut self, keycode: u16, pressed: bool) -> CGEventFlags {
        let Some(flag) = modifier_flag_for_keycode(keycode) else {
            return self.modifier_flags;
        };

        if pressed {
            self.modifier_flags.insert(flag);
            self.pressed_modifiers.insert(keycode);
        } else {
            self.modifier_flags.remove(flag);
            self.pressed_modifiers.remove(&keycode);
        }

        self.modifier_flags
    }
}

fn modifier_flag_for_keycode(keycode: u16) -> Option<CGEventFlags> {
    match keycode {
        0x38 | 0x3C => Some(CGEventFlags::CGEventFlagShift),
        0x3B | 0x3E => Some(CGEventFlags::CGEventFlagControl),
        0x3A | 0x3D => Some(CGEventFlags::CGEventFlagAlternate),
        0x37 | 0x36 => Some(CGEventFlags::CGEventFlagCommand),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_keycodes_map_to_core_graphics_flags() {
        assert_eq!(
            modifier_flag_for_keycode(0x38),
            Some(CGEventFlags::CGEventFlagShift)
        );
        assert_eq!(
            modifier_flag_for_keycode(0x3B),
            Some(CGEventFlags::CGEventFlagControl)
        );
        assert_eq!(
            modifier_flag_for_keycode(0x3A),
            Some(CGEventFlags::CGEventFlagAlternate)
        );
        assert_eq!(
            modifier_flag_for_keycode(0x37),
            Some(CGEventFlags::CGEventFlagCommand)
        );
        assert_eq!(modifier_flag_for_keycode(0x05), None);
    }
}
