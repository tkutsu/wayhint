//! What wayhint does at a target: the verbs. Pure data, so the config can
//! bind them, the selection can arm them and the pointer can carry them
//! out without any of the three knowing about the others.

/// One pointer action.
///
/// `clap::ValueEnum` lets `--verb` take these by name (`--verb right`); it
/// lower-cases the variant names, the same names [`Action::name`] gives the
/// config fields, and a test keeps the two in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
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
}

impl Action {
    /// Every action, in the order `wayhint config` prints them.
    pub const ALL: [Self; 7] = [
        Self::Left,
        Self::Right,
        Self::Middle,
        Self::Double,
        Self::Hover,
        Self::Back,
        Self::Forward,
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
        }
    }

    /// Whether it presses a button. A press on a text field focuses it, so
    /// the run ends there and typing goes to the app; a hover doesn't.
    pub const fn clicks(self) -> bool {
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

    /// Both sets together: Ctrl held with Shift is both.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self {
            shift: self.shift || other.shift,
            ctrl: self.ctrl || other.ctrl,
            alt: self.alt || other.alt,
        }
    }
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
