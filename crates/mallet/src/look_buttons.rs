//! Mouse button presses during mouselook.
//!
//! In mouselook the cursor is hidden and held inside the window. The windowing layer only
//! passes a button press on while it knows where the cursor is, and with the cursor hidden
//! it can lose track of it, so clicks through the crosshair would do nothing. On Windows
//! the buttons are therefore also read from the system each frame, and a press counts once
//! whether the window's events or the system saw it first. Elsewhere only the window's
//! events are used.

/// The left and right mouse buttons, in that order.
pub type Buttons = [bool; 2];

/// Tracks the mouse buttons across frames while mouselook is on.
#[derive(Debug, Default)]
pub struct LookButtons {
    /// Which buttons the system reported down on the previous frame.
    was_down: Buttons,
    /// Which buttons already gave a press while they are held, so the same press is not
    /// taken twice when the window's event arrives a frame after the system saw it.
    used: Buttons,
    /// Which buttons are down this frame, by either source.
    down: Buttons,
}

impl LookButtons {
    /// Reads one frame during mouselook. `event_pressed` and `event_down` are what the
    /// window's events say this frame. The system is only asked while the window has the
    /// `focus`, so clicks in other programs never count. Returns which buttons were pressed
    /// this frame.
    pub fn frame(&mut self, event_pressed: Buttons, event_down: Buttons, focus: bool) -> Buttons {
        let system = if focus { system_buttons_down() } else { None };
        let now = system.unwrap_or([false; 2]);
        let mut pressed = [false; 2];
        for button in 0..2 {
            let edge = now[button] && !self.was_down[button];
            pressed[button] = (edge || event_pressed[button]) && !self.used[button];
            self.used[button] = now[button] && (self.used[button] || pressed[button]);
            self.down[button] = now[button] || event_down[button];
        }
        self.was_down = now;
        pressed
    }

    /// Forgets the buttons while mouselook is off, so a button already held when it starts
    /// does not count as a press.
    pub fn idle(&mut self) {
        self.was_down = system_buttons_down().unwrap_or([false; 2]);
        self.used = self.was_down;
        self.down = [false; 2];
    }

    /// Which buttons are held this frame.
    pub fn down(&self) -> Buttons {
        self.down
    }
}

/// The left and right buttons as the system has them now, with swapped buttons taken into
/// account; `None` where the system cannot be asked.
#[cfg(windows)]
fn system_buttons_down() -> Option<Buttons> {
    const VK_LBUTTON: i32 = 0x01;
    const VK_RBUTTON: i32 = 0x02;
    const SM_SWAPBUTTON: i32 = 23;

    #[link(name = "user32")]
    extern "system" {
        fn GetAsyncKeyState(key: i32) -> i16;
        fn GetSystemMetrics(index: i32) -> i32;
    }

    // SAFETY: both functions take a plain integer and only read system state.
    let (left, right, swapped) = unsafe {
        (
            GetAsyncKeyState(VK_LBUTTON) < 0,
            GetAsyncKeyState(VK_RBUTTON) < 0,
            GetSystemMetrics(SM_SWAPBUTTON) != 0,
        )
    };
    Some(if swapped { [right, left] } else { [left, right] })
}

#[cfg(not(windows))]
fn system_buttons_down() -> Option<Buttons> {
    None
}
