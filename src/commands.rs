//! One table drives the key bindings, the menu bar, the command palette and
//! the keyboard-shortcuts sheet, so they can never disagree.

use gpui::{Action, KeyBinding, actions};

use crate::editor::*;

actions!(
    tallypad,
    [
        Quit,
        ToggleCommandPalette,
        ShowShortcuts,
        ShowAbout,
        RefreshRates,
        CheckForUpdates,
        OpenRepo,
        Dismiss,
        PaletteUp,
        PaletteDown,
        PaletteConfirm,
        PaletteBackspace,
        PalettePaste,
        ToggleGraph,
        UseOpus,
        UseSonnet,
        UseHaiku,
        SetApiKey,
        RemoveApiKey,
    ]
);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Menu {
    File,
    Edit,
    View,
    Ai,
    Help,
}

impl Menu {
    pub const ALL: [Menu; 5] = [Menu::File, Menu::Edit, Menu::View, Menu::Ai, Menu::Help];

    pub fn label(self) -> &'static str {
        match self {
            Menu::File => "File",
            Menu::Edit => "Edit",
            Menu::View => "View",
            Menu::Ai => "AI",
            Menu::Help => "Help",
        }
    }
}

pub struct Command {
    pub menu: Menu,
    pub name: &'static str,
    /// Key bindings; the first one is shown in menus and the palette.
    pub keys: &'static [&'static str],
    pub action: fn() -> Box<dyn Action>,
    /// Draw a separator above this entry in its menu.
    pub separator: bool,
}

macro_rules! command_table {
    ($( $(#[$sep:ident])? $menu:ident $name:literal => $action:expr, [$($key:literal),*], $ctx:expr; )*) => {
        pub fn all() -> Vec<Command> {
            vec![$(
                Command {
                    menu: Menu::$menu,
                    name: $name,
                    keys: &[$($key),*],
                    action: || -> Box<dyn Action> { Box::new($action) },
                    separator: command_table!(@sep $($sep)?),
                },
            )*]
        }

        pub fn bindings() -> Vec<KeyBinding> {
            let mut out = Vec::new();
            $( $( out.push(KeyBinding::new($key, $action, $ctx)); )* )*
            out
        }
    };
    (@sep sep) => { true };
    (@sep) => { false };
}

const ED: Option<&str> = Some("Editor");

command_table! {
    File "New Sheet" => NewSheet, ["ctrl-n"], ED;
    File "Open…" => Open, ["ctrl-o"], ED;
    File "Save" => Save, ["ctrl-s"], ED;
    File "Save As…" => SaveAs, ["ctrl-shift-s"], ED;
    #[sep] File "Copy Sheet with Results" => CopyWithResults, ["ctrl-shift-c"], ED;
    #[sep] File "Quit" => Quit, ["ctrl-q"], None;

    Edit "Undo" => Undo, ["ctrl-z"], ED;
    Edit "Redo" => Redo, ["ctrl-y", "ctrl-shift-z"], ED;
    #[sep] Edit "Cut" => Cut, ["ctrl-x", "shift-delete"], ED;
    Edit "Copy" => Copy, ["ctrl-c", "ctrl-insert"], ED;
    Edit "Paste" => Paste, ["ctrl-v", "shift-insert"], ED;
    #[sep] Edit "Select All" => SelectAll, ["ctrl-a"], ED;

    View "Command Palette" => ToggleCommandPalette, ["ctrl-k", "ctrl-shift-p"], None;
    #[sep] View "Zoom In" => ZoomIn, ["ctrl-=", "ctrl-+"], ED;
    View "Zoom Out" => ZoomOut, ["ctrl--"], ED;
    View "Reset Zoom" => ZoomReset, ["ctrl-0"], ED;
    #[sep] View "Show Graph" => ToggleGraph, ["ctrl-g"], None;

    Ai "Ask Claude Again" => AskAi, ["ctrl-enter"], ED;
    #[sep] Ai "Use Claude Opus 5.5" => UseOpus, [], None;
    Ai "Use Claude Sonnet 5.5" => UseSonnet, [], None;
    Ai "Use Claude Haiku 5.5" => UseHaiku, [], None;
    #[sep] Ai "Set API Key…" => SetApiKey, [], None;
    Ai "Remove API Key" => RemoveApiKey, [], None;

    Help "Keyboard Shortcuts" => ShowShortcuts, ["f1", "ctrl-/"], None;
    #[sep] Help "Refresh Exchange Rates" => RefreshRates, ["ctrl-r"], None;
    Help "Check for Updates…" => CheckForUpdates, [], None;
    #[sep] Help "Tallypad on GitHub" => OpenRepo, [], None;
    Help "About Tallypad" => ShowAbout, [], None;
}

/// Editing keys that aren't menu commands (shown on the shortcuts sheet).
pub const EDITING_KEYS: &[(&str, &str)] = &[
    ("Ask Claude", "start a line with ?"),
    ("Complete a variable", "tab"),
    ("Move by word", "ctrl-left / ctrl-right"),
    ("Select", "shift + arrows"),
    ("Delete word", "ctrl-backspace"),
    ("Line start / end", "home / end"),
    ("Top / bottom", "ctrl-home / ctrl-end"),
    ("Copy a result", "click it"),
    ("Copy the total", "click the footer"),
];

pub fn editing_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("backspace", Backspace, ED),
        KeyBinding::new("shift-backspace", Backspace, ED),
        KeyBinding::new("delete", Delete, ED),
        KeyBinding::new("ctrl-backspace", DeleteWordLeft, ED),
        KeyBinding::new("ctrl-delete", DeleteWordRight, ED),
        KeyBinding::new("left", Left, ED),
        KeyBinding::new("right", Right, ED),
        KeyBinding::new("up", Up, ED),
        KeyBinding::new("down", Down, ED),
        KeyBinding::new("ctrl-left", WordLeft, ED),
        KeyBinding::new("ctrl-right", WordRight, ED),
        KeyBinding::new("shift-left", SelectLeft, ED),
        KeyBinding::new("shift-right", SelectRight, ED),
        KeyBinding::new("shift-up", SelectUp, ED),
        KeyBinding::new("shift-down", SelectDown, ED),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, ED),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, ED),
        KeyBinding::new("home", Home, ED),
        KeyBinding::new("end", End, ED),
        KeyBinding::new("shift-home", SelectHome, ED),
        KeyBinding::new("shift-end", SelectEnd, ED),
        KeyBinding::new("ctrl-home", DocStart, ED),
        KeyBinding::new("ctrl-end", DocEnd, ED),
        KeyBinding::new("ctrl-shift-home", SelectDocStart, ED),
        KeyBinding::new("ctrl-shift-end", SelectDocEnd, ED),
        KeyBinding::new("pageup", PageUp, ED),
        KeyBinding::new("pagedown", PageDown, ED),
        KeyBinding::new("enter", Newline, ED),
        KeyBinding::new("shift-enter", Newline, ED),
        KeyBinding::new("tab", Tab, ED),
        KeyBinding::new("escape", CancelCompletion, ED),
        KeyBinding::new("escape", Dismiss, Some("Workspace")),
        KeyBinding::new("up", PaletteUp, Some("Palette")),
        KeyBinding::new("down", PaletteDown, Some("Palette")),
        KeyBinding::new("enter", PaletteConfirm, Some("Palette")),
        KeyBinding::new("backspace", PaletteBackspace, Some("Palette")),
        KeyBinding::new("ctrl-v", PalettePaste, Some("Palette")),
    ]
}

/// "ctrl-shift-s" -> "Ctrl+Shift+S", "ctrl--" -> "Ctrl+-".
pub fn key_label(keys: &str) -> String {
    keys.split(" / ")
        .map(|chord| {
            let chord = chord.trim();
            let (mods, key) = if let Some(prefix) = chord.strip_suffix("--") {
                (prefix, "-")
            } else if chord == "-" {
                ("", "-")
            } else {
                match chord.rsplit_once('-') {
                    Some((m, k)) => (m, k),
                    None => ("", chord),
                }
            };
            let mut parts: Vec<String> = mods
                .split('-')
                .filter(|m| !m.is_empty())
                .map(|m| match m {
                    "ctrl" => "Ctrl".to_string(),
                    "shift" => "Shift".to_string(),
                    "alt" => "Alt".to_string(),
                    other => other.to_string(),
                })
                .collect();
            let key = match key {
                "delete" => "Del".to_string(),
                "insert" => "Ins".to_string(),
                "backspace" => "Backspace".to_string(),
                "pageup" => "PgUp".to_string(),
                "pagedown" => "PgDn".to_string(),
                k if k.chars().count() == 1
                    || (k.starts_with('f') && k[1..].parse::<u8>().is_ok()) =>
                {
                    k.to_uppercase()
                }
                k => {
                    let mut c = k.chars();
                    c.next()
                        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                        .unwrap_or_default()
                }
            };
            parts.push(key);
            parts.join("+")
        })
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Fuzzy subsequence score; `None` when `query` doesn't match.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let text_l = text.to_lowercase();
    let query_l = query.to_lowercase();
    if let Some(pos) = text_l.find(&query_l) {
        // Contiguous matches win, earlier ones more so.
        return Some(1000 - pos as i32);
    }
    let mut score = 0;
    let mut last: Option<usize> = None;
    let chars: Vec<char> = text_l.chars().collect();
    let mut i = 0;
    for q in query_l.chars().filter(|c| !c.is_whitespace()) {
        loop {
            if i >= chars.len() {
                return None;
            }
            if chars[i] == q {
                let at_word = i == 0 || !chars[i - 1].is_alphanumeric();
                score += if at_word { 15 } else { 5 };
                if last.is_some_and(|l| l + 1 == i) {
                    score += 10;
                }
                last = Some(i);
                i += 1;
                break;
            }
            i += 1;
        }
    }
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(key_label("ctrl-shift-s"), "Ctrl+Shift+S");
        assert_eq!(key_label("ctrl--"), "Ctrl+-");
        assert_eq!(key_label("f1"), "F1");
        assert_eq!(key_label("shift-delete"), "Shift+Del");
        assert_eq!(key_label("home / end"), "Home / End");
    }

    #[test]
    fn fuzzy() {
        assert!(fuzzy_score("sva", "Save As…").is_some());
        assert!(fuzzy_score("zz", "Save").is_none());
        assert!(fuzzy_score("save", "Save").unwrap() > fuzzy_score("sve", "Save").unwrap());
    }

    #[test]
    fn table_builds() {
        let cmds = all();
        assert!(
            cmds.iter()
                .any(|c| c.name == "Command Palette" && c.keys[0] == "ctrl-k")
        );
        let keys: usize = cmds.iter().map(|c| c.keys.len()).sum();
        assert_eq!(bindings().len(), keys);
        assert!(cmds.iter().any(|c| c.menu == Menu::Ai));
    }
}
