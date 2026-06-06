use ironrdp_server::{KeyboardEvent, MouseEvent, RdpServerInputHandler};
use macrdp_input::{
    InputTapLocation, KeyboardInjector, KeyboardInjectorConfig, MouseButton, MouseInjector,
    MouseInjectorConfig,
};
use std::sync::{Arc, Mutex};

/// Maps RDP desktop coordinates to macOS logical coordinates.
///
/// Completely independent of capture/encode resolution — only cares about
/// the RDP desktop size (what the client thinks) and the macOS logical
/// display size (what CGEvent expects).
///
/// Formula: `mac = rdp * logical / rdp_desktop`
#[derive(Debug, Clone)]
pub struct MouseCoordMapper(Arc<Mutex<CoordState>>);

#[derive(Debug)]
struct CoordState {
    logical_w: f64,
    logical_h: f64,
    rdp_w: f64,
    rdp_h: f64,
}

impl MouseCoordMapper {
    pub fn new(logical_w: u16, logical_h: u16, rdp_w: u16, rdp_h: u16) -> Self {
        Self(Arc::new(Mutex::new(CoordState {
            logical_w: logical_w as f64,
            logical_h: logical_h as f64,
            rdp_w: rdp_w.max(1) as f64,
            rdp_h: rdp_h.max(1) as f64,
        })))
    }

    pub fn map(&self, rdp_x: u16, rdp_y: u16) -> (u16, u16) {
        let s = self.0.lock().unwrap();
        let mac_x = (rdp_x as f64 * s.logical_w / s.rdp_w) as u16;
        let mac_y = (rdp_y as f64 * s.logical_h / s.rdp_h) as u16;
        (mac_x, mac_y)
    }

    pub fn update_rdp_size(&self, rdp_w: u16, rdp_h: u16) {
        let mut s = self.0.lock().unwrap();
        s.rdp_w = rdp_w.max(1) as f64;
        s.rdp_h = rdp_h.max(1) as f64;
    }

    #[cfg(test)]
    pub fn scale(&self) -> (f64, f64) {
        let s = self.0.lock().unwrap();
        (s.rdp_w / s.logical_w, s.rdp_h / s.logical_h)
    }
}

/// Bridges RDP input events to macOS CGEvent injection
pub struct MacInputHandler {
    keyboard: Option<KeyboardInjector>,
    mouse: Option<MouseInjector>,
    last_mouse_x: u16,
    last_mouse_y: u16,
    left_pressed: bool,
    right_pressed: bool,
    middle_pressed: bool,
    coord_mapper: MouseCoordMapper,
}

impl MacInputHandler {
    pub fn new(coord_mapper: MouseCoordMapper) -> Self {
        Self::new_with_tap_location(coord_mapper, InputTapLocation::default())
    }

    pub fn new_with_tap_location(
        coord_mapper: MouseCoordMapper,
        tap_location: InputTapLocation,
    ) -> Self {
        let keyboard = KeyboardInjector::new_with_config(KeyboardInjectorConfig { tap_location })
            .map_err(|e| tracing::error!("Failed to create keyboard injector: {e}"))
            .ok();
        let mouse = MouseInjector::new_with_config(MouseInjectorConfig { tap_location })
            .map_err(|e| tracing::error!("Failed to create mouse injector: {e}"))
            .ok();

        if keyboard.is_none() || mouse.is_none() {
            tracing::warn!("Input injection may fail — ensure Accessibility permission is granted");
        }
        tracing::info!(?tap_location, "Input injection tap location configured");

        Self {
            keyboard,
            mouse,
            last_mouse_x: 0,
            last_mouse_y: 0,
            left_pressed: false,
            right_pressed: false,
            middle_pressed: false,
            coord_mapper,
        }
    }

    fn inject_button(&mut self, button: MouseButton, pressed: bool) -> anyhow::Result<()> {
        let Some(mouse) = &self.mouse else {
            return Ok(());
        };

        mouse.button_event(button, pressed, self.last_mouse_x, self.last_mouse_y)?;
        self.set_button_state(button, pressed);
        Ok(())
    }

    fn set_button_state(&mut self, button: MouseButton, pressed: bool) {
        match button {
            MouseButton::Left => self.left_pressed = pressed,
            MouseButton::Right => self.right_pressed = pressed,
            MouseButton::Middle => self.middle_pressed = pressed,
        }
    }

    fn reset_mouse_buttons(&mut self) {
        let pressed_buttons = [
            (MouseButton::Left, self.left_pressed),
            (MouseButton::Right, self.right_pressed),
            (MouseButton::Middle, self.middle_pressed),
        ];

        for (button, pressed) in pressed_buttons {
            if !pressed {
                continue;
            }

            let result = self.mouse.as_ref().map_or(Ok(()), |mouse| {
                mouse.button_event(button, false, self.last_mouse_x, self.last_mouse_y)
            });
            if let Err(e) = result {
                tracing::warn!(?button, "Mouse button release during reset failed: {e}");
            }
            self.set_button_state(button, false);
        }
    }
}

impl RdpServerInputHandler for MacInputHandler {
    fn keyboard(&mut self, event: KeyboardEvent) {
        let Some(kb) = &mut self.keyboard else { return };

        let result = match event {
            KeyboardEvent::Pressed { code, extended } => kb.inject_key(code, extended, true),
            KeyboardEvent::Released { code, extended } => kb.inject_key(code, extended, false),
            KeyboardEvent::UnicodePressed(ch) => kb.inject_unicode(ch, true),
            KeyboardEvent::UnicodeReleased(ch) => kb.inject_unicode(ch, false),
            KeyboardEvent::Synchronize(_flags) => kb.reset_modifiers(),
        };

        if let Err(e) = result {
            tracing::warn!("Keyboard injection failed: {e}");
        }
    }

    fn mouse(&mut self, event: MouseEvent) {
        let result = match event {
            MouseEvent::Move { x, y } => {
                let (mx, my) = self.coord_mapper.map(x, y);
                self.last_mouse_x = mx;
                self.last_mouse_y = my;
                self.mouse
                    .as_ref()
                    .map_or(Ok(()), |mouse| mouse.move_to(mx, my))
            }
            MouseEvent::LeftPressed => self.inject_button(MouseButton::Left, true),
            MouseEvent::LeftReleased => self.inject_button(MouseButton::Left, false),
            MouseEvent::RightPressed => self.inject_button(MouseButton::Right, true),
            MouseEvent::RightReleased => self.inject_button(MouseButton::Right, false),
            MouseEvent::MiddlePressed => self.inject_button(MouseButton::Middle, true),
            MouseEvent::MiddleReleased => self.inject_button(MouseButton::Middle, false),
            MouseEvent::Scroll { x, y } => self
                .mouse
                .as_ref()
                .map_or(Ok(()), |mouse| mouse.scroll(x, y)),
            _ => {
                tracing::trace!(?event, "Unhandled mouse event");
                Ok(())
            }
        };

        if let Err(e) = result {
            tracing::warn!("Mouse injection failed: {e}");
        }
    }

    fn reset(&mut self) {
        self.reset_mouse_buttons();

        if let Some(kb) = &mut self.keyboard {
            if let Err(e) = kb.reset_modifiers() {
                tracing::warn!("Keyboard modifier reset failed: {e}");
            }
        }
    }
}
