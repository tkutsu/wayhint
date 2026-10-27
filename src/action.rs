//! What wayhint does at a target: the verbs, and the whole command they
//! are part of. Pure data, so the config can bind them, the selection can
//! arm them and the pointer can carry them out without any of the three
//! knowing about the others.

/// One pointer action.
///
/// `clap::ValueEnum` lets `--verb` take these by name (`--verb right`); it
/// lower-cases the variant names, the same names [`Action::name`] gives the
/// config fields, and a test keeps the two in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum Action {
    Left,
    Right,
    Middle,
    /// Two left clicks, close enough together to count as one double click.
    Double,
    /// Move there and stay, for tooltips and hover menus. Never clicks.
    Hover,
    /// The mouse's back button (`BTN_SIDE`): a browser goes back.
    Back,
    /// The mouse's forward button (`BTN_EXTRA`).
    Forward,
    /// The wheel, `[pointer] scroll` notches up.
    #[value(name = "scroll_up")]
    ScrollUp,
    /// The wheel, `[pointer] scroll` notches down.
    #[value(name = "scroll_down")]
    ScrollDown,
    /// Press the left button and keep it down: the next command lets go
    /// where it points, so a drag, or a long press where it was.
    Hold,
}

impl Action {
    /// Every action, in the order `wayhint config` prints them.
    pub const ALL: [Self; 10] = [
        Self::Left,
        Self::Right,
        Self::Middle,
        Self::Double,
        Self::Hover,
        Self::Back,
        Self::Forward,
        Self::ScrollUp,
        Self::ScrollDown,
        Self::Hold,
    ];

    /// The action's `[keys]` field, and what the status pill shows.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Middle => "middle",
            Self::Double => "double",
            Self::Hover => "hover",
            Self::Back => "back",
            Self::Forward => "forward",
            Self::ScrollUp => "scroll_up",
            Self::ScrollDown => "scroll_down",
            Self::Hold => "hold",
        }
    }

    /// What it does, in a few words, for the comments `wayhint config`
    /// writes.
    pub const fn about(self) -> &'static str {
        match self {
            Self::Left => "left click",
            Self::Right => "right click",
            Self::Middle => "middle click",
            Self::Double => "double click",
            Self::Hover => "move there and stay, for tooltips",
            Self::Back => "the mouse's back button",
            Self::Forward => "the mouse's forward button",
            Self::ScrollUp => "the wheel up, [pointer] scroll notches",
            Self::ScrollDown => "the wheel down, [pointer] scroll notches",
            Self::Hold => "press and keep down; the next tag or Space lets go there",
        }
    }

    /// What it does in a word or two, for the help key's legend.
    pub const fn short(self) -> &'static str {
        match self {
            Self::Left => "left click",
            Self::Right => "right click",
            Self::Middle => "middle click",
            Self::Double => "double click",
            Self::Hover => "hover",
            Self::Back => "back button",
            Self::Forward => "forward button",
            Self::ScrollUp => "scroll up",
            Self::ScrollDown => "scroll down",
            Self::Hold => "hold, then drop on the next",
        }
    }

    /// Whether it presses a button. A press on a text field focuses it, so
    /// the run ends there and typing goes to the app; a hover or a scroll
    /// doesn't.
    pub const fn clicks(self) -> bool {
        !matches!(self, Self::Hover | Self::ScrollUp | Self::ScrollDown)
    }

    /// Whether Shift, Ctrl and Alt are held through it: a Ctrl-scroll
    /// zooms. A hover holds nothing, and a hold keeps its button down past
    /// the command, which a held modifier mustn't.
    pub const fn takes_mods(self) -> bool {
        !matches!(self, Self::Hover | Self::Hold)
    }

    /// Whether the cursor may go back afterwards (`[pointer] after`): not
    /// from a hover, which is there to stay.
    pub const fn restores(self) -> bool {
        !matches!(self, Self::Hover)
    }
}

/// Modifiers held through a click: Shift-click, Ctrl-click, Alt-click, or
/// any mix. Held on a tag's last letter (or on `Space`), they go to the app
/// through a virtual keyboard around the click.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Mods {
    pub const NONE: Self = Self {
        shift: false,
        ctrl: false,
        alt: false,
    };

    pub const fn is_empty(self) -> bool {
        !self.shift && !self.ctrl && !self.alt
    }
}

/// One command, all but where: the action and the modifiers held through
/// it. A tag, a match or the pointer gives the point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Act {
    pub action: Action,
    pub mods: Mods,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum as _;

    #[test]
    fn config_names_and_cli_names_agree() {
        for action in Action::ALL {
            let cli = action.to_possible_value().unwrap();
            assert_eq!(cli.get_name(), action.name());
        }
        assert_eq!(Action::value_variants().len(), Action::ALL.len());
    }
}
