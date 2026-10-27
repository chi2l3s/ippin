//! Presentation stays outside the gatekeeper library and its network policy.
use clap::{
    ColorChoice,
    builder::styling::{AnsiColor, Style, Styles},
};
use std::{fmt::Display, io::IsTerminal, sync::OnceLock};

static INTERACTIVE: OnceLock<bool> = OnceLock::new();

#[derive(Clone, Copy)]
pub enum Tone {
    Success,
    Warning,
    Danger,
    Accent,
    Muted,
}

impl Tone {
    fn style(self) -> Style {
        match self {
            Self::Success => AnsiColor::Green.on_default().bold(),
            Self::Warning => AnsiColor::Yellow.on_default().bold(),
            Self::Danger => AnsiColor::Red.on_default().bold(),
            Self::Accent => AnsiColor::Cyan.on_default().bold(),
            Self::Muted => Style::new().dimmed(),
        }
    }
}

pub const fn help_styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::Cyan.on_default().bold())
        .usage(AnsiColor::Cyan.on_default().bold())
        .literal(Style::new().bold())
        .placeholder(AnsiColor::Cyan.on_default())
        .valid(AnsiColor::Green.on_default())
        .invalid(AnsiColor::Red.on_default())
        .error(AnsiColor::Red.on_default().bold())
}

/// Resolve early so even clap's help and parser errors honor --color.
pub fn color_choice(args: &[std::ffi::OsString]) -> ColorChoice {
    let mut choice = ColorChoice::Auto;
    let mut args = args.iter().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        let text = arg.to_string_lossy();
        let value = if text == "--color" {
            args.next().map(|v| v.to_string_lossy())
        } else {
            text.strip_prefix("--color=")
                .map(std::borrow::Cow::Borrowed)
        };
        match value.as_deref() {
            Some("always") => choice = ColorChoice::Always,
            Some("never") => choice = ColorChoice::Never,
            Some("auto") => choice = ColorChoice::Auto,
            _ => {}
        }
    }
    choice
}

pub fn init(choice: ColorChoice) {
    let stream_choice = match choice {
        ColorChoice::Always => anstream::ColorChoice::Always,
        ColorChoice::Never => anstream::ColorChoice::Never,
        _ => anstream::ColorChoice::Auto,
    };
    stream_choice.write_global();
    let _ = INTERACTIVE.set(std::io::stdout().is_terminal());
}

pub fn interactive() -> bool {
    *INTERACTIVE.get().unwrap_or(&false)
}

pub fn colors_enabled() -> bool {
    anstream::AutoStream::auto(std::io::stderr()).current_choice() != anstream::ColorChoice::Never
}

pub fn title(action: &str) {
    if !interactive() {
        return;
    }
    let brand = Tone::Accent.style();
    let muted = Tone::Muted.style();
    anstream::println!(
        "\n  {brand}IPPin{brand:#}  {muted}v{}{muted:#}  /  {action}",
        env!("CARGO_PKG_VERSION")
    );
    anstream::println!("  {muted}────────────────────────────────────────────────{muted:#}");
}

pub fn section(label: &str) {
    if interactive() {
        let style = Tone::Muted.style();
        anstream::println!("\n  {style}{}{style:#}", label.to_ascii_uppercase());
    }
}

pub fn field(label: &str, value: impl Display) {
    if interactive() {
        let muted = Tone::Muted.style();
        anstream::println!("  {muted}{label:<18}{muted:#} {value}");
    } else {
        anstream::println!("{label}: {value}");
    }
}

pub fn state(label: &str, value: &str, detail: impl Display, tone: Tone) {
    let detail = detail.to_string();
    let style = tone.style();
    if interactive() {
        let muted = Tone::Muted.style();
        anstream::println!("  {muted}{label:<18}{muted:#} {style}● {value}{style:#}  {detail}");
    } else {
        let suffix = if detail.is_empty() {
            String::new()
        } else {
            format!(" ({detail})")
        };
        anstream::println!("{label}: {style}{value}{style:#}{suffix}");
    }
}

pub fn success(text: impl Display) {
    if interactive() {
        let style = Tone::Success.style();
        anstream::println!("\n  {style}✓{style:#} {text}");
    } else {
        anstream::println!("{text}");
    }
}

pub fn hint(text: impl Display) {
    if interactive() {
        let style = Tone::Muted.style();
        anstream::println!("  {style}{text}{style:#}");
    } else {
        anstream::println!("{text}");
    }
}

pub fn warning(text: impl Display) {
    let style = Tone::Warning.style();
    let indent = if interactive() { "\n  " } else { "" };
    anstream::println!("{indent}{style}Note:{style:#} {text}");
}

pub fn error(text: impl Display) {
    let style = Tone::Danger.style();
    anstream::eprintln!("{style}Error:{style:#} {text}");
}

pub fn log_line(line: &str) {
    let tone = if line.contains("BLOCK") || line.contains("KILL") || line.contains("ERROR") {
        Tone::Danger
    } else if line.contains("PASS") {
        Tone::Success
    } else {
        Tone::Muted
    };
    let style = tone.style();
    anstream::println!("{style}{line}{style:#}");
}
