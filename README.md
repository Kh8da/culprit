<p align="center">
  <img src="src-tauri/app-icon.png" width="96" alt="">
</p>

<h1 align="center">culprit</h1>

<p align="center">Your game lagged or stuttered. Was it your Wi-Fi, your PC, or the server?</p>

<br>

**culprit** turns a play session into a verdict: what caused each stutter and lag spike, the evidence, and what
to do about it.

> **Status: work in progress.** The analysis engine, the app and its report screens are built and tested. The
> Windows recorder is written and its logic is tested, but it is new and still needs trying on real games and
> PCs. There is no download yet.

## What a report tells you

- **One verdict up top**, like "Microsoft Defender caused 9 of your 11 stutters." or "Your Wi-Fi caused all 11
  of your lag spikes."
- **The causes, ranked**, each with how sure culprit is, the evidence across the session, and a fix.
- **Every moment**, with the numbers: "One frame took 118 ms (normally 6 ms)", "Microsoft Defender jumped to
  41% of your CPU (it usually uses under 1%)."
- **A timeline** of frame times and pings to your router, your provider and the game server, with every hitch
  marked.

## How it decides

- **Stutters.** Every second, each possible cause is either active or not: another app in a CPU or disk burst,
  the CPU maxed out or slowed down, RAM or GPU memory full, the GPU throttling. A cause is only blamed when it
  is active far more often during stutters than the rest of the time, so an app that is always a little busy
  never gets blamed for hitches it didn't cause. When nothing stands out, the frame's own CPU and GPU time tell
  shader-compile hitches from heavy GPU frames.
- **Lag.** The router, the provider's first hop and the game server are pinged side by side. If the router is
  slow too, the trouble is your home network or Wi-Fi. If only the provider's hop is slow, it's the provider.
  If both are fine, it's the server or the route to it. An app using lots of bandwidth at the same moment is
  checked first.
- **What it ignores.** The slow frames right after alt-tabbing back in, loading screens, the server going
  quiet when a match ends, and single stray pings that routers drop on purpose.

## What it records, and what it never does

Press Start recording, play, press Stop. Windows asks for administrator rights when a recording starts, because
only administrators may trace frames and network traffic. Culprit then reads what Windows already reports to any monitoring tool:

- **Frames** from [PresentMon](https://github.com/GameTechDev/PresentMon), Intel's frame tracer, which runs next
  to Culprit as its own program. Culprit locks on to the game in front once it draws 20 fps or more for two
  seconds.
- **Network traffic** from Windows' own network trace: which program sent or received how many bytes, and to
  which address. That is how Culprit finds the game server. No packet's contents are read.
- **The whole PC once a second**, like Task Manager: CPU, memory, disk, GPU use, temperature and throttling,
  Wi-Fi signal, and every program's CPU, disk and network use.
- **Pings** to your router, your provider's first hop and the game server, four a second.
- **Ctrl+Alt+L** marks a moment, through a registered hotkey.

Nothing is injected into the game and nothing reads its memory. Culprit opens no other process at all and
installs no keyboard hook and no driver; PresentMon opens each program that draws only with the query-only
access Task Manager uses, to read its name. The pings, and the traceroute that finds your provider's hop (pings
with a hop limit), are the only packets Culprit sends. The PC is sampled once a second and frames and packets are
only counted, so the recorder stays light while you play.

## Fast on purpose

- A three-hour session at 240 fps (2.6 million frames, 120 background apps) is analyzed in under half a second.
- The app is plain HTML, CSS and JavaScript with canvas charts and no framework. The release binary is about
  4.6 MB.
- Reports are sent to the screen as the saved bytes, never decoded and re-encoded on the way.

## Layout

| Folder | What it is |
|---|---|
| `core` | The analysis engine (`culpritcore`): detection, cause attribution, wording, charts, example sessions. |
| `capture` | The recorder: reads frames, packets, pings and the PC on Windows, works out which program is the game and which address is its server, and builds the recording. |
| `src-tauri` | The desktop app: session storage and the commands the UI calls. |
| `ui` | The app's screens. |
| `ui-dev` | A stand-in backend for previewing the UI in a browser, and the UI check. |

## Build and test

```
cargo test --workspace
npm ci
npx tauri dev
```

To preview the UI in a browser with the example sessions:

```
cargo run --release -p culpritcore --example export_demo -- ui-dev/sessions
npx http-server -p 8765 -s .
```

Then open `http://127.0.0.1:8765/ui/index.html`. Add `?record` to the address to try the live view with fake
data. `node ui-dev/check.js` walks through every screen in Chromium and fails on any error.

To record from a dev build on Windows, put
[PresentMon-2.6.0-x64.exe](https://github.com/GameTechDev/PresentMon/releases/tag/v2.6.0) next to the built
`culprit.exe` (or point `CULPRIT_PRESENTMON` at it) and start `npx tauri dev` from an administrator terminal. The
CI build ships PresentMon next to `Culprit.exe`.

Every run appends to a plain text log at `%APPDATA%\app.culprit.lagfinder\logs\culprit.log`: what the PC and
connection look like, whether frame capture, the network trace, pings, the hotkey and the GPU sensors started,
the first raw network events, what PresentMon printed, a status line every 10 seconds and any crash. It holds
the PC's hardware, program names and the IP addresses of your router, provider and game server, nothing else.

## License

MIT. Geist and Geist Mono are under the SIL Open Font License (`ui/fonts/OFL.txt`). PresentMon is Intel's, under
the MIT license (`licenses/PresentMon.txt`).
