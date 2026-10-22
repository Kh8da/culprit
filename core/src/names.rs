//! Names people recognise for the programs and services that commonly get in a game's way.

/// Friendly name for an executable, matched case-insensitively on the file name.
pub fn app_label(exe: &str) -> Option<&'static str> {
    let exe = exe.to_ascii_lowercase();
    let label = match exe.as_str() {
        "msmpeng.exe" | "mpdefendercoreservice.exe" | "nissrv.exe" | "mpcmdrun.exe" => "Microsoft Defender",
        "tiworker.exe" | "trustedinstaller.exe" => "Windows Update (installing)",
        "wuauclt.exe" | "usoclient.exe" | "musnotification.exe" | "musnotifyicon.exe" => "Windows Update",
        "mousocoreworker.exe" => "Windows Update",
        "searchindexer.exe" | "searchprotocolhost.exe" | "searchfilterhost.exe" => "Windows Search indexing",
        "compattelrunner.exe" | "devicecensus.exe" => "Windows telemetry",
        "msiexec.exe" => "Windows Installer",
        "wmiprvse.exe" => "Windows Management (WMI)",
        "dllhost.exe" => "Windows COM host",
        "explorer.exe" => "Windows Explorer",
        "dwm.exe" => "Windows desktop compositor",
        "csrss.exe" | "system" | "registry" | "smss.exe" | "wininit.exe" | "services.exe" | "lsass.exe" => {
            "Windows core"
        }
        "memory compression" => "Windows memory compression",
        "audiodg.exe" => "Windows audio",
        "onedrive.exe" | "filecoauth.exe" => "OneDrive",
        "dropbox.exe" => "Dropbox",
        "googledrivefs.exe" => "Google Drive",
        "chrome.exe" => "Google Chrome",
        "msedge.exe" => "Microsoft Edge",
        "msedgewebview2.exe" => "Edge WebView (used by many apps)",
        "firefox.exe" => "Firefox",
        "opera.exe" | "opera_gx.exe" => "Opera",
        "brave.exe" => "Brave",
        "vivaldi.exe" => "Vivaldi",
        "discord.exe" | "discordcanary.exe" | "discordptb.exe" => "Discord",
        "spotify.exe" => "Spotify",
        "steam.exe" => "Steam",
        "steamwebhelper.exe" => "Steam (overlay and store)",
        "steamservice.exe" => "Steam Service",
        "epicgameslauncher.exe" | "epicwebhelper.exe" => "Epic Games Launcher",
        "battle.net.exe" | "agent.exe" => "Battle.net",
        "eadesktop.exe" | "eabackgroundservice.exe" => "EA app",
        "upc.exe" | "uplaywebcore.exe" => "Ubisoft Connect",
        "galaxyclient.exe" | "galaxyclient helper.exe" => "GOG Galaxy",
        "riotclientservices.exe" | "riotclientux.exe" => "Riot Client",
        "xboxpcapp.exe" | "gamingservices.exe" => "Xbox app",
        "obs64.exe" | "obs32.exe" => "OBS Studio",
        "medal.exe" => "Medal",
        "nvcontainer.exe" | "nvidia share.exe" | "nvidia overlay.exe" | "nvidia app.exe" => "NVIDIA app and overlay",
        "nvdisplay.container.exe" => "NVIDIA display driver service",
        "radeonsoftware.exe" | "amdrsserv.exe" | "amdow.exe" => "AMD Software",
        "armourycrate.service.exe" | "armourycrate.usersessionhelper.exe" | "asusoptimization.exe" => "Armoury Crate",
        "icue.exe" | "corsair.service.exe" => "Corsair iCUE",
        "lghub.exe" | "lghub_agent.exe" | "lghub_updater.exe" => "Logitech G HUB",
        "razer synapse 3.exe" | "razer synapse service.exe" | "razercentralservice.exe" => "Razer Synapse",
        "signalrgb.exe" => "SignalRGB",
        "wallpaper64.exe" | "wallpaper32.exe" | "webwallpaper32.exe" => "Wallpaper Engine",
        "teams.exe" | "ms-teams.exe" => "Microsoft Teams",
        "slack.exe" => "Slack",
        "zoom.exe" => "Zoom",
        "code.exe" => "Visual Studio Code",
        "qbittorrent.exe" => "qBittorrent",
        "utorrent.exe" | "bittorrent.exe" => "uTorrent",
        "avastsvc.exe" | "avastui.exe" => "Avast",
        "avgsvc.exe" | "avgui.exe" => "AVG",
        "mcshield.exe" | "mfemms.exe" | "mfevtps.exe" => "McAfee",
        "ccsvchst.exe" | "nortonsecurity.exe" => "Norton",
        "bdagent.exe" | "vsserv.exe" | "bdservicehost.exe" => "Bitdefender",
        "avp.exe" => "Kaspersky",
        "ekrn.exe" => "ESET",
        "malwarebytes.exe" | "mbamservice.exe" => "Malwarebytes",
        "culprit.exe" | "culprit-cli.exe" => "Culprit (this app)",
        _ if exe.starts_with("presentmon") => "Culprit (frame capture)",
        _ => return None,
    };
    Some(label)
}

/// Friendly name for a Windows service, by its short service name.
pub fn service_label(service: &str) -> Option<&'static str> {
    let label = match service.to_ascii_lowercase().as_str() {
        "wuauserv" | "usosvc" | "waasmedicsvc" => "Windows Update",
        "dosvc" => "Windows Update downloads (Delivery Optimization)",
        "bits" => "Background downloads (BITS)",
        "wsearch" => "Windows Search indexing",
        "sysmain" => "SysMain (app preloading)",
        "diagtrack" => "Windows telemetry",
        "windefend" | "wdnissvc" => "Microsoft Defender",
        "schedule" => "Scheduled tasks",
        "winmgmt" => "Windows Management (WMI)",
        "cryptsvc" => "Windows cryptography and update checks",
        "trustedinstaller" => "Windows Update (installing)",
        "audiosrv" | "audioendpointbuilder" => "Windows audio",
        "dnscache" => "DNS client",
        "wlansvc" => "Wi-Fi (WLAN AutoConfig)",
        "netprofm" | "nlasvc" => "Network detection",
        "storsvc" => "Storage service",
        "defragsvc" => "Drive optimisation (defrag/trim)",
        "msiserver" => "Windows Installer",
        "fontcache" => "Font cache",
        "themes" => "Themes",
        "pcasvc" => "Program Compatibility Assistant",
        "lfsvc" => "Location service",
        "xblgamesave" | "xblauthmanager" | "xboxnetapisvc" => "Xbox Live services",
        "gamingservices" | "gamingservicesnet" => "Xbox Gaming Services",
        _ => return None,
    };
    Some(label)
}

/// Programs that draw frames but are never the game the player means, so automatic game
/// detection skips them. The player can still pick one by hand.
pub fn never_the_game(exe: &str) -> bool {
    matches!(
        exe.to_ascii_lowercase().as_str(),
        "dwm.exe"
            | "explorer.exe"
            | "culprit.exe"
            | "culprit-cli.exe"
            | "chrome.exe"
            | "msedge.exe"
            | "msedgewebview2.exe"
            | "firefox.exe"
            | "opera.exe"
            | "opera_gx.exe"
            | "brave.exe"
            | "vivaldi.exe"
            | "discord.exe"
            | "discordcanary.exe"
            | "discordptb.exe"
            | "spotify.exe"
            | "steam.exe"
            | "steamwebhelper.exe"
            | "epicgameslauncher.exe"
            | "epicwebhelper.exe"
            | "battle.net.exe"
            | "eadesktop.exe"
            | "upc.exe"
            | "galaxyclient.exe"
            | "riotclientux.exe"
            | "riotclientservices.exe"
            | "xboxpcapp.exe"
            | "obs64.exe"
            | "obs32.exe"
            | "medal.exe"
            | "nvidia overlay.exe"
            | "nvidia app.exe"
            | "radeonsoftware.exe"
            | "code.exe"
            | "slack.exe"
            | "teams.exe"
            | "ms-teams.exe"
            | "zoom.exe"
            | "applicationframehost.exe"
            | "textinputhost.exe"
            | "searchhost.exe"
            | "searchapp.exe"
            | "startmenuexperiencehost.exe"
            | "shellexperiencehost.exe"
            | "lockapp.exe"
            | "wallpaper64.exe"
            | "wallpaper32.exe"
            | "webwallpaper32.exe"
    ) || exe.to_ascii_lowercase().starts_with("presentmon")
}

/// "cs2.exe" → "cs2", for when nothing better is known.
pub fn exe_stem(exe: &str) -> &str {
    let base = exe.rsplit(['\\', '/']).next().unwrap_or(exe);
    match base.len().checked_sub(4) {
        Some(i) if base.is_char_boundary(i) && base[i..].eq_ignore_ascii_case(".exe") => &base[..i],
        _ => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(app_label("MsMpEng.exe"), Some("Microsoft Defender"));
        assert_eq!(app_label("PresentMon-2.6.0-x64.exe"), Some("Culprit (frame capture)"));
        assert_eq!(app_label("cs2.exe"), None);
        assert_eq!(service_label("DoSvc"), Some("Windows Update downloads (Delivery Optimization)"));
        assert!(never_the_game("Discord.exe"));
        assert!(!never_the_game("cs2.exe"));
        assert_eq!(exe_stem(r"C:\Games\cs2.EXE"), "cs2");
        assert_eq!(exe_stem("game"), "game");
        assert_eq!(exe_stem("é.exe"), "é");
    }
}
