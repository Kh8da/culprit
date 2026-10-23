//! What to call the game, each background program and the PC, from what Windows knows
//! about them.

use culpritcore::model::ProcessKind;
use culpritcore::names;

/// A running Windows service.
#[derive(Clone, Debug, PartialEq)]
pub struct Service {
    /// The short name, e.g. "wuauserv".
    pub name: String,
    /// What Windows shows for it, e.g. "Windows Update".
    pub display: String,
}

/// Words that mark an engine, build or launcher name rather than what a person calls the program.
const GENERIC_PARTS: &[&str] = &[
    "shipping",
    "win64",
    "win32",
    "x64",
    "dx11",
    "dx12",
    "vulkan",
    "launcher",
    "bootstrap",
    "unreal",
    "ue4",
    "ue5",
    "unity",
    "client",
    ".exe",
];
const GENERIC_NAMES: &[&str] = &["game", "application", "app", "main", "unknown"];

/// `s` cleaned up, if it reads like a name a person would recognise.
pub fn usable_name(s: &str) -> Option<String> {
    let mut s = s.to_string();
    for mark in ["™", "®", "©", "(TM)", "(tm)", "(R)", "(r)"] {
        s = s.replace(mark, " ");
    }
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = s.to_lowercase();
    let generic = GENERIC_PARTS.iter().any(|g| lower.contains(g)) || GENERIC_NAMES.contains(&lower.as_str());
    (!s.is_empty() && s.chars().count() <= 60 && !generic && s.chars().any(char::is_alphanumeric)).then_some(s)
}

/// The useful part of a window title: "Minecraft 1.21 - Multiplayer" becomes "Minecraft 1.21".
fn title_name(title: &str) -> Option<String> {
    let first =
        [" - ", " | ", " – ", " — ", " (C)", " (c)"].iter().fold(title, |t, sep| t.split(sep).next().unwrap_or(t));
    usable_name(first)
}

/// What to call the game: the exe's own description or product name, else its window
/// title, else the exe name without ".exe".
pub fn game_name(exe: &str, description: Option<&str>, product: Option<&str>, title: Option<&str>) -> String {
    description
        .and_then(usable_name)
        .or_else(|| product.and_then(usable_name))
        .or_else(|| title.and_then(title_name))
        .unwrap_or_else(|| names::exe_stem(exe).to_string())
}

/// Grouping key, label and kind for a process, as the collector groups them. Programs are
/// grouped by exe name; a shared service host by the service it runs. `describe` looks up
/// the exe's own description and is only called when no better name is known.
pub fn identify(
    exe: &str,
    services: &[Service],
    describe: impl FnOnce() -> Option<String>,
) -> (String, String, ProcessKind) {
    if exe.eq_ignore_ascii_case("svchost.exe") && !services.is_empty() {
        let svc = services
            .iter()
            .find(|s| names::service_label(&s.name).is_some())
            .or_else(|| services.iter().min_by_key(|s| s.name.to_ascii_lowercase()))
            .unwrap_or(&services[0]);
        let label = names::service_label(&svc.name)
            .map(str::to_string)
            .or_else(|| usable_name(&svc.display))
            .unwrap_or_else(|| svc.name.clone());
        return (format!("svc:{}", svc.name.to_ascii_lowercase()), label, ProcessKind::Service);
    }
    let label = match names::app_label(exe) {
        Some(l) => l.to_string(),
        None => describe().as_deref().and_then(usable_name).unwrap_or_else(|| exe.to_string()),
    };
    (exe.to_ascii_lowercase(), label, ProcessKind::App)
}

/// "Windows 11 24H2", from the build number and the version Windows shows in Settings.
pub fn os_name(build: u32, display_version: Option<&str>) -> String {
    let base = if build >= 22_000 { "Windows 11" } else { "Windows 10" };
    match display_version.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => format!("{base} {v}"),
        None => base.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_names() {
        assert_eq!(game_name("cs2.exe", Some("Counter-Strike 2"), None, None), "Counter-Strike 2");
        assert_eq!(game_name("eldenring.exe", Some("ELDEN RING™"), None, None), "ELDEN RING");
        assert_eq!(
            game_name(
                "FortniteClient-Win64-Shipping.exe",
                Some("FortniteClient-Win64-Shipping"),
                Some("Fortnite"),
                None
            ),
            "Fortnite"
        );
        assert_eq!(game_name("RustClient.exe", Some("RustClient"), Some("Unity Player"), Some("Rust")), "Rust");
        assert_eq!(
            game_name("javaw.exe", Some(""), None, Some("Minecraft* 1.21 - Multiplayer (3rd-party Server)")),
            "Minecraft* 1.21"
        );
        assert_eq!(
            game_name("League of Legends.exe", Some("League of Legends (TM) Client"), None, None),
            "League of Legends"
        );
        assert_eq!(game_name("game.exe", Some("Game"), None, Some("   ")), "game");
    }

    #[test]
    fn groups_services_and_apps() {
        let svc = |n: &str, d: &str| Service { name: n.into(), display: d.into() };
        let (key, label, kind) = identify("svchost.exe", &[svc("wuauserv", "Windows Update")], || panic!("not needed"));
        assert_eq!((key.as_str(), label.as_str(), kind), ("svc:wuauserv", "Windows Update", ProcessKind::Service));
        // Several services in one host: the one Culprit knows wins, else the first by name.
        let (key, ..) =
            identify("svchost.exe", &[svc("Zeta", "Z"), svc("BITS", "Background"), svc("Alpha", "A")], || None);
        assert_eq!(key, "svc:bits");
        let (key, label, _) =
            identify("svchost.exe", &[svc("Zeta", "Zeta Thing"), svc("Alpha", "Alpha Thing")], || None);
        assert_eq!((key.as_str(), label.as_str()), ("svc:alpha", "Alpha Thing"));

        assert_eq!(identify("MsMpEng.exe", &[svc("WinDefend", "x")], || None).0, "msmpeng.exe");
        assert_eq!(identify("MsMpEng.exe", &[], || None).1, "Microsoft Defender");
        assert_eq!(identify("notepad.exe", &[], || Some("Notepad".into())).1, "Notepad");
        assert_eq!(identify("x.exe", &[], || Some("x64 Launcher".into())).1, "x.exe");
        assert_eq!(identify("svchost.exe", &[], || None).0, "svchost.exe");
    }

    #[test]
    fn os_names() {
        assert_eq!(os_name(26_100, Some("24H2")), "Windows 11 24H2");
        assert_eq!(os_name(19_045, None), "Windows 10");
    }
}
