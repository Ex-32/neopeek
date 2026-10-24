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

    /// Present ⇔ `--dump-fft` was given (per-slice feature readout lines).
    #[arg(
        long,
        value_name = "SECONDS",
        default_missing_value = "2.0",
        num_args = 0..=1,
        help = "run the FFT pipeline on SECONDS (default 2) of capture and print features"
    )]
    pub dump_fft: Option<f64>,

    /// Capture source by name (see --list-devices); default is the default
    /// sink's monitor ("what the host hears").
    #[arg(long, value_name = "SOURCE", help = "capture source by name")]
    pub device: Option<String>,

    /// Backend override: `pulse` (default, covers PipeWire hosts via
    /// pipewire-pulse); `alsa` fallback lands later (PLAN.md §3.3).
    #[arg(long = "backend", value_name = "BACKEND", default_value = "pulse")]
    pub backend: String,

    /// Slow continuous scene yaw, sndpeek's classic spin behavior
    /// (PLAN.md §4.3). Off by default.
    #[arg(long, help = "slow continuous yaw (sndpeek-style autospin)")]
    pub autospin: bool,

    /// Theme: built-in name (`Ember`/`Ice`/`Terminal`) or path to a
    /// `.ron` theme file (PLAN.md §6.2). Missing/malformed → Ember.
    #[arg(long, value_name = "NAME|FILE", help = "theme name or .ron path")]
    pub theme: Option<String>,

    /// Re-parse --theme's file on every change (dev aid). Off by default.
    #[arg(long, help = "hot-reload the loaded theme file")]
    pub watch_theme: bool,
}

impl Cli {
    /// True when a non-visual CLI action was requested.
    pub fn has_cli_action(&self) -> bool {
        self.list_devices || self.dump_pcm.is_some() || self.dump_fft.is_some()
    }

    /// Seconds to dump when `--dump-pcm` is set (default 2).
    pub fn dump_seconds(&self) -> f64 {
        self.dump_pcm.unwrap_or(2.0)
    }

    /// Seconds to feed the FFT pipeline when `--dump-fft` is set (default 2).
    pub fn dump_fft_seconds(&self) -> f64 {
        self.dump_fft.unwrap_or(2.0)
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

        let fft = Cli::try_parse_from(["neopeek", "--dump-fft"]).unwrap();
        assert_eq!(fft.dump_fft_seconds(), 2.0);
        assert!(fft.has_cli_action());

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
