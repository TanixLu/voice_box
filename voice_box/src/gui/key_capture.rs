//! Interactive capture of the next key combination the user presses.

use crate::{hotkeys, model::Hotkey, soundboard::bindings::Target};

struct Capture {
    target: Target,
    armed: bool,
    modifier: Option<Hotkey>,
}

#[derive(Default)]
pub struct KeyCapture(Option<Capture>);

impl KeyCapture {
    pub fn target(&self) -> Option<&Target> {
        self.0.as_ref().map(|capture| &capture.target)
    }
    pub fn cancel(&mut self) {
        self.0 = None;
    }
    pub fn begin(&mut self, target: Target) {
        self.0 = Some(Capture {
            target,
            armed: false,
            modifier: None,
        });
    }

    /// The key pressed for the current capture, once it is complete.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<(Target, Hotkey)> {
        let capture = self.0.as_mut()?;
        ctx.request_repaint_after(std::time::Duration::from_millis(20));
        if !ctx.input(|input| input.focused)
            || ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.cancel();
            return None;
        }
        let pressed = hotkeys::pressed();
        let modifier = hotkeys::pressed_modifier();
        if !capture.armed {
            capture.armed = pressed.is_none() && modifier.is_none();
            return None;
        }
        let pressed = if capture.target == Target::Hold && pressed.is_none() {
            if modifier.is_some() {
                capture.modifier = modifier;
                return None;
            }
            capture.modifier.take()
        } else {
            pressed
        };
        let key = pressed?;
        let target = self.0.take()?.target;
        if key.is_escape() {
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            return None;
        }
        Some((target, key))
    }
}
