use anyhow::Result;
use core_foundation_sys::dictionary::{CFDictionaryRef, CFMutableDictionaryRef};
use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
#[allow(deprecated)]
use objc2_io_kit::IOHIDPostEvent;
use objc2_io_kit::{
    io_connect_t, io_service_t, kIOHIDParamConnectType, kIOHIDSystemClass, kNXEventDataVersion,
    IOGPoint, IOHIDAccessType, IOHIDCheckAccess, IOHIDRequestAccess, IOHIDRequestType,
    IOObjectRelease, IOServiceClose, IOServiceOpen, NXEventData, NXEventData_compound,
    NXEventData_compound_misc,
};
use std::collections::BTreeSet;
use std::ffi::c_char;

use crate::keymap::{scancode_to_action, KeyAction, MediaKey};
use crate::InputTapLocation;

const NX_SUBTYPE_AUX_CONTROL_BUTTONS: i16 = 8;
const NX_SYSDEFINED: u32 = 14;
const NX_KEY_DOWN: i32 = 0x0a;
const NX_KEY_UP: i32 = 0x0b;
const NX_KEYTYPE_SOUND_UP: i32 = 0;
const NX_KEYTYPE_SOUND_DOWN: i32 = 1;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    fn IOServiceGetMatchingService(
        main_port: libc::mach_port_t,
        matching: CFDictionaryRef,
    ) -> io_service_t;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct KeyboardInjectorConfig {
    pub tap_location: InputTapLocation,
}

pub struct KeyboardInjector {
    modifier_flags: CGEventFlags,
    pressed_modifiers: BTreeSet<u16>,
    tap_location: InputTapLocation,
    media_hid: Option<MediaHidConnection>,
}

impl KeyboardInjector {
    pub fn new() -> Result<Self> {
        Self::new_with_config(KeyboardInjectorConfig::default())
    }

    pub fn new_with_config(config: KeyboardInjectorConfig) -> Result<Self> {
        // Verify we can create an event source (permission check)
        let _ = CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|_| {
            anyhow::anyhow!("Failed to create CGEventSource — check Accessibility permission")
        })?;
        Ok(Self {
            modifier_flags: CGEventFlags::CGEventFlagNull,
            pressed_modifiers: BTreeSet::new(),
            tap_location: config.tap_location,
            media_hid: None,
        })
    }

    fn source() -> Result<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| anyhow::anyhow!("Failed to create CGEventSource"))
    }

    /// Inject a key press or release event
    pub fn inject_key(&mut self, scancode: u8, extended: bool, pressed: bool) -> Result<()> {
        let action = match scancode_to_action(scancode, extended) {
            Some(action) => action,
            None => {
                tracing::warn!(scancode, extended, "Unknown scancode, ignoring");
                return Ok(());
            }
        };

        match action {
            KeyAction::MacKeyCode(keycode) => self.inject_keycode(scancode, keycode, pressed),
            KeyAction::Media(media_key) => self.inject_media_key(media_key, pressed),
        }
    }

    fn inject_keycode(&mut self, scancode: u8, keycode: u16, pressed: bool) -> Result<()> {
        let source = Self::source()?;
        let event = CGEvent::new_keyboard_event(source, keycode as CGKeyCode, pressed)
            .map_err(|_| anyhow::anyhow!("Failed to create keyboard event"))?;

        let flags = self.flags_for_event(keycode, pressed);
        event.set_flags(flags);
        event.post(self.cg_event_tap_location());
        tracing::trace!(scancode, keycode, pressed, "Keyboard event injected");
        Ok(())
    }

    fn inject_media_key(&mut self, media_key: MediaKey, pressed: bool) -> Result<()> {
        if self.media_hid.is_none() {
            self.media_hid = Some(MediaHidConnection::open()?);
        }

        let nx_key_type = media_key.nx_key_type();
        let key_state = if pressed { NX_KEY_DOWN } else { NX_KEY_UP };
        let data1 = (nx_key_type << 16) | (key_state << 8);
        let mut long_data = [0_i32; 11];
        long_data[0] = data1;
        long_data[1] = -1;

        let event_data = NXEventData {
            compound: NXEventData_compound {
                reserved: 0,
                subType: NX_SUBTYPE_AUX_CONTROL_BUTTONS,
                misc: NXEventData_compound_misc { L: long_data },
            },
        };
        let connect = self
            .media_hid
            .as_ref()
            .expect("media_hid initialized above")
            .raw();

        #[allow(deprecated)]
        let status = unsafe {
            IOHIDPostEvent(
                connect,
                NX_SYSDEFINED,
                IOGPoint { x: 0, y: 0 },
                &event_data,
                kNXEventDataVersion,
                0,
                0,
            )
        };
        if status != 0 {
            self.media_hid = None;
            anyhow::bail!("IOHIDPostEvent failed for {media_key:?}: status={status}");
        }
        tracing::trace!(
            ?media_key,
            pressed,
            "Media key event injected via IOHIDPostEvent"
        );
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

        event.post(self.cg_event_tap_location());
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
                event.post(self.cg_event_tap_location());
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

    fn cg_event_tap_location(&self) -> CGEventTapLocation {
        self.tap_location.as_cg_event_tap_location()
    }
}

impl MediaKey {
    fn nx_key_type(self) -> i32 {
        match self {
            Self::VolumeUp => NX_KEYTYPE_SOUND_UP,
            Self::VolumeDown => NX_KEYTYPE_SOUND_DOWN,
        }
    }
}

struct MediaHidConnection {
    connect: io_connect_t,
}

impl MediaHidConnection {
    fn open() -> Result<Self> {
        ensure_hid_post_access();

        let matching = unsafe { IOServiceMatching(kIOHIDSystemClass.as_ptr()) };
        if matching.is_null() {
            anyhow::bail!("IOServiceMatching failed for IOHIDSystem");
        }

        let service = unsafe { IOServiceGetMatchingService(0, matching as CFDictionaryRef) };
        if service == 0 {
            anyhow::bail!("IOServiceGetMatchingService failed for IOHIDSystem");
        }

        let mut connect = 0;
        #[allow(deprecated)]
        let owning_task = unsafe { libc::mach_task_self() };
        let status =
            unsafe { IOServiceOpen(service, owning_task, kIOHIDParamConnectType, &mut connect) };
        let release_status = IOObjectRelease(service);
        if release_status != 0 {
            tracing::warn!(
                status = release_status,
                "IOObjectRelease failed for IOHIDSystem service"
            );
        }
        if status != 0 {
            anyhow::bail!("IOServiceOpen failed for IOHIDSystem: status={status}");
        }
        if connect == 0 {
            anyhow::bail!("IOServiceOpen returned a null IOHIDSystem connection");
        }

        tracing::trace!("IOHIDSystem connection opened for media key injection");
        Ok(Self { connect })
    }

    fn raw(&self) -> io_connect_t {
        self.connect
    }
}

impl Drop for MediaHidConnection {
    fn drop(&mut self) {
        let status = IOServiceClose(self.connect);
        if status != 0 {
            tracing::warn!(status, "IOServiceClose failed for IOHIDSystem connection");
        }
    }
}

fn ensure_hid_post_access() {
    let access = IOHIDCheckAccess(IOHIDRequestType::PostEvent);
    if access == IOHIDAccessType::Granted {
        return;
    }

    if access == IOHIDAccessType::Denied {
        tracing::warn!(
            "IOHID post-event access is denied; enable Input Monitoring for the server process"
        );
        return;
    }

    let granted = IOHIDRequestAccess(IOHIDRequestType::PostEvent);
    if granted {
        tracing::info!("IOHID post-event access granted");
    } else {
        tracing::warn!(
            "IOHID post-event access is not granted; media keys may require Input Monitoring"
        );
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
