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

    /// sndpeek `--print` parity: run capture + FFT on SECONDS (default 2)
    /// and print one space-separated line per slice:
    /// `centroid_hz flux rms ro50_hz ro80_hz`.
    #[arg(
        long,
        value_name = "SECONDS",
        default_missing_value = "2.0",
        num_args = 0..=1,
        help = "print SECONDS (default 2) of per-slice features and exit"
    )]
    pub print: Option<f64>,

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

    /// Waterfall history layers; `--depth` maps to the same setting the
    /// `PageUp`/`PageDown` keys adjust at runtime.
    #[arg(long, value_name = "N", help = "waterfall history layers (8-256, default 48)")]
    pub depth: Option<usize>,

    /// Theme: built-in name (`Ember`/`Ice`/`Terminal`/`Cyber`) or path to a
    /// `.ron` theme file (PLAN.md §6.2). Missing/malformed → Ember.
    #[arg(long, value_name = "NAME|FILE", help = "theme name or .ron path")]
    pub theme: Option<String>,

    /// Re-parse --theme's file on every change (dev aid). Off by default.
    #[arg(long, help = "hot-reload the loaded theme file")]
    pub watch_theme: bool,
}

impl Cli {
    /// Waterfall history layers; default 48, clamped to 8…256 — the same
    /// clamp `neopeek_render::grid` applies to the runtime depth keys
    /// (kept literal here so neopeek-config stays dependency-light).
    pub fn depth(&self) -> usize {
        self.depth.unwrap_or(48).clamp(8, 256)
    }

    /// True when a non-visual CLI action was requested.
    pub fn has_cli_action(&self) -> bool {
        self.list_devices || self.dump_pcm.is_some() || self.dump_fft.is_some()
            || self.print.is_some()
    }

    /// Seconds to dump when `--dump-pcm` is set (default 2).
    pub fn dump_seconds(&self) -> f64 {
        self.dump_pcm.unwrap_or(2.0)
    }

    /// Seconds to feed the FFT pipeline when `--dump-fft` is set (default 2).
    pub fn dump_fft_seconds(&self) -> f64 {
        self.dump_fft.unwrap_or(2.0)
    }

    /// Seconds to feed the pipeline when `--print` is set (default 2).
    pub fn print_seconds(&self) -> f64 {
        self.print.unwrap_or(2.0)
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

    /// `--depth`: parsed and clamped to the 8…256 range like the runtime
    /// `PageUp`/`PageDown` keys.
    #[test]
    fn cli_parses_depth_clamped() {
        let none = Cli::try_parse_from(["neopeek"]).unwrap();
        assert_eq!(none.depth(), 48);

        let d = Cli::try_parse_from(["neopeek", "--depth", "96"]).unwrap();
        assert_eq!(d.depth, Some(96));
        assert_eq!(d.depth(), 96);

        let low = Cli::try_parse_from(["neopeek", "--depth", "2"]).unwrap();
        assert_eq!(low.depth(), 8);

        let high = Cli::try_parse_from(["neopeek", "--depth", "999999"]).unwrap();
        assert_eq!(high.depth(), 256);

        assert!(Cli::try_parse_from(["neopeek", "--depth"]).is_err());
        assert!(Cli::try_parse_from(["neopeek", "--depth", "x"]).is_err());
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

        let print_bare = Cli::try_parse_from(["neopeek", "--print"]).unwrap();
        assert_eq!(print_bare.print, Some(2.0));
        assert!(print_bare.has_cli_action(), "--print is an action");
        assert_eq!(print_bare.print_seconds(), 2.0);

        let print_explicit = Cli::try_parse_from(["neopeek", "--print", "1.5"]).unwrap();
        assert_eq!(print_explicit.print_seconds(), 1.5);

        // Explicit (but zero) seconds is still an action.
        let zero = Cli::try_parse_from(["neopeek", "--print", "0"]).unwrap();
        assert!(zero.has_cli_action());
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
