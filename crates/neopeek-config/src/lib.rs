//! neopeek-config: CLI parsing (clap), TOML app config, XDG paths.
//!
//! M2 scope: the command-line surface (PLAN.md §3.5). TOML config file
//! loading lands with theming/config milestones; for now only CLI.

pub use clap::Parser;

/// neopeek: modern sndpeek-style audio visualizer.
#[derive(Parser, Debug)]
#[command(name = "neopeek", version, about)]
pub struct Cli {
    /// List capture sources (mics + sink monitors) and exit.
    #[arg(long, help = "list capture sources and exit")]
    pub list_devices: bool,

    /// Present ⇔ `--dump-pcm` was given (bare = 2 s, or explicit seconds).
    #[arg(
        long,
        value_name = "SECONDS",
        default_missing_value = "2.0",
        num_args = 0..=1,
        help = "dump SECONDS (default 2) of f32 PCM to stdout and exit"
    )]
    pub dump_pcm: Option<f64>,

    /// Capture source by name (see --list-devices); default is the default
    /// sink's monitor ("what the host hears").
    #[arg(long, value_name = "SOURCE", help = "capture source by name")]
    pub device: Option<String>,

    /// Backend override: `pulse` (default, covers PipeWire hosts via
    /// pipewire-pulse); `alsa` fallback lands later (PLAN.md §3.3).
    #[arg(long = "backend", value_name = "BACKEND", default_value = "pulse")]
    pub backend: String,
}

impl Cli {
    /// True when a non-visual CLI action was requested.
    pub fn has_cli_action(&self) -> bool {
        self.list_devices || self.dump_pcm.is_some()
    }

    /// Seconds to dump when `--dump-pcm` is set (default 2).
    pub fn dump_seconds(&self) -> f64 {
        self.dump_pcm.unwrap_or(2.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_parses_defaults() {
        let cli = Cli::try_parse_from(["neopeek"]).unwrap();
        assert!(!cli.list_devices);
        assert_eq!(cli.dump_pcm, None);
        assert!(cli.device.is_none());
        assert_eq!(cli.backend, "pulse");
        assert!(!cli.has_cli_action());
        assert_eq!(cli.dump_seconds(), 2.0);
    }

    #[test]
    fn cli_parses_actions() {
        let bare = Cli::try_parse_from(["neopeek", "--dump-pcm"]).unwrap();
        assert_eq!(bare.dump_pcm, Some(2.0));
        assert!(bare.has_cli_action());
        assert_eq!(bare.dump_seconds(), 2.0);

        let explicit = Cli::try_parse_from(["neopeek", "--dump-pcm", "4"]).unwrap();
        assert_eq!(explicit.dump_seconds(), 4.0);

        let listed = Cli::try_parse_from(["neopeek", "--list-devices"]).unwrap();
        assert!(listed.list_devices);

        let dev =
            Cli::try_parse_from(["neopeek", "--device", "alsa_output.pci-0000.monitor"]).unwrap();
        assert_eq!(dev.device.as_deref(), Some("alsa_output.pci-0000.monitor"));
    }

    #[test]
    fn cli_help_renders() {
        let mut cmd = Cli::command();
        let help = cmd.render_help().to_string();
        assert!(help.contains("--list-devices"));
        assert!(help.contains("--device"));
        assert!(help.contains("--dump-pcm"));
    }
}
