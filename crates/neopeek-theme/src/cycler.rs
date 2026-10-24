//! Rotating through the built-ins plus a loaded custom theme (PLAN.md
//! §6.2 "T": built-ins → custom → built-ins; wraps).

/// Current built-in order: Ember → Ice → Terminal → Cyber (custom appended).
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

    /// Replaces the entry sourced at `path` (the watched custom theme).
    /// Returns `true` when replaced — `false` means the cycler holds no
    /// file theme matching the path (caller logs).
    pub fn reload(&mut self, path: &std::path::Path, theme: crate::Theme) -> bool {
        let entry = self
            .entries
            .iter_mut()
            .find(|l| l.path.as_deref() == Some(path));
        match entry {
            Some(l) => {
                l.theme = theme;
                true
            }
            None => false,
        }
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
        assert_eq!(c.cycle().theme.name, "Cyber");
        assert_eq!(c.cycle().theme.name, "Ember"); // wrapped, Ember not duped
    }

    #[test]
    fn reload_replaces_only_the_file_entry() {
        let p = std::path::PathBuf::from("/tmp/nope.ron");
        let mut c = Cycler::new(custom("MyStuff")); // path == /tmp/nope.ron
        // Advance to Ice, then reload the file entry.
        c.cycle(); // Ember
        let mut t = crate::builtin("Terminal").unwrap();
        t.name = "Tweaked".to_owned();
        assert!(c.reload(&p, t.clone()));
        // Cycle until the custom slot: it must now hold the reloaded theme.
        loop {
            if c.cycle().theme.name == "Tweaked" {
                break;
            }
        }
        // Reload of an unknown path fails cleanly.
        assert!(!c.reload(std::path::Path::new("/tmp/other.ron"), t.clone()));
    }

    #[test]
    fn custom_theme_appended_after_builtins() {
        // Starting at a custom theme: cycling runs the built-ins first,
        // then returns to the custom one.
        let mut c = Cycler::new(custom("MyStuff"));
        assert_eq!(c.cycle().theme.name, "Ember");
        assert_eq!(c.cycle().theme.name, "Ice");
        assert_eq!(c.cycle().theme.name, "Terminal");
        assert_eq!(c.cycle().theme.name, "Cyber");
        assert_eq!(c.cycle().theme.name, "MyStuff");
        assert_eq!(c.cycle().theme.name, "Ember");
    }
}
