//! Rotating through the built-ins plus a loaded custom theme (PLAN.md
//! §6.2 "T": built-ins → custom → built-ins; wraps).

/// Current built-in order: Ember → Ice → Terminal (custom appended).
pub struct Cycler {
    pos: usize,
    entries: Vec<crate::LoadedTheme>,
}

impl Cycler {
    /// Starts at `initial`; the remaining built-ins follow (deduped by
    /// name, so a built-in start doesn't appear twice).
    pub fn new(initial: crate::LoadedTheme) -> Self {
        let mut entries = vec![initial];
        for name in crate::BUILTIN_NAMES {
            let dupe = entries.iter().any(|l| {
                l.path.is_none() && l.theme.name.eq_ignore_ascii_case(name)
            });
            if !dupe {
                entries.push(crate::LoadedTheme::builtin(name));
            }
        }
        Self { pos: 0, entries }
    }

    /// Advances and wraps; returns the newly selected theme source.
    pub fn cycle(&mut self) -> &crate::LoadedTheme {
        self.pos = (self.pos + 1) % self.entries.len();
        &self.entries[self.pos]
    }

    /// The currently selected theme source.
    pub fn current(&self) -> &crate::LoadedTheme {
        &self.entries[self.pos]
    }
}

impl Default for Cycler {
    fn default() -> Self {
        Self::new(crate::LoadedTheme::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LoadedTheme;

    fn custom(name: &str) -> crate::LoadedTheme {
        let theme = crate::builtin("Ember").unwrap();
        let mut theme = theme;
        theme.name = name.to_owned();
        crate::LoadedTheme { theme, path: Some(std::path::PathBuf::from("/tmp/nope.ron")) }
    }

    #[test]
    fn cycles_builtins_then_wraps() {
        let mut c = Cycler::new(LoadedTheme::builtin("Ember"));
        assert_eq!(c.cycle().theme.name, "Ice");
        assert_eq!(c.cycle().theme.name, "Terminal");
        assert_eq!(c.cycle().theme.name, "Ember"); // wrapped, Ember not duped
    }

    #[test]
    fn custom_theme_appended_after_builtins() {
        // Starting at a custom theme: cycling runs the built-ins first,
        // then returns to the custom one.
        let mut c = Cycler::new(custom("MyStuff"));
        assert_eq!(c.cycle().theme.name, "Ember");
        assert_eq!(c.cycle().theme.name, "Ice");
        assert_eq!(c.cycle().theme.name, "Terminal");
        assert_eq!(c.cycle().theme.name, "MyStuff");
        assert_eq!(c.cycle().theme.name, "Ember");
    }
}
