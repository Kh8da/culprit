<p align="center">
  <img src="src-tauri/app-icon.png" width="96" alt="">
</p>

<h1 align="center">culprit</h1>

<p align="center">Your game lagged or stuttered. Was it your Wi-Fi, your PC, or the server?</p>

<br>

**culprit** turns a play session into a verdict: what caused each stutter and lag spike, the evidence, and what
to do about it.

> **Status: work in progress.** The analysis engine, the app and its report screens are built and tested. The
> Windows recorder that collects the data during a real game isn't written yet, so this build shows reports and
> an example, but can't record. There is no download yet.

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

## Fast on purpose

- A three-hour session at 240 fps (2.6 million frames, 120 background apps) is analyzed in under half a second.
- The app is plain HTML, CSS and JavaScript with canvas charts and no framework. The release binary is about
  4.6 MB.
- Reports are sent to the screen as the saved bytes, never decoded and re-encoded on the way.

## Layout

| Folder | What it is |
|---|---|
| `core` | The analysis engine (`culpritcore`): detection, cause attribution, wording, charts, example sessions. |
| `capture` | Builds a recording from raw events and works out which program is the game and which address is its server. |
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

## License

MIT. Geist and Geist Mono are under the SIL Open Font License (`ui/fonts/OFL.txt`).
