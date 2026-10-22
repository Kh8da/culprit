# culprit

Your game lagged or stuttered. Was it your Wi-Fi, your PC, or the server? **culprit** records a play session
and names the cause of each hitch, with the evidence and what to do about it.

> **Status: work in progress.** The analysis engine is built and tested. The Windows recorder that collects the
> data on a real PC, and the app around it, aren't written yet, so there is nothing to download.

## What's here

| Crate | What it does |
|---|---|
| `core` (`culpritcore`) | Takes a recorded session and produces the verdict. It finds stutters (frames much slower than the ones just before) and lag spikes (slow or lost pings on the way to the game server, or the server going quiet). Then it decides what caused each one. |
| `capture` (`culprit-capture`) | Turns raw events (frames, packets, pings, a once-a-second sample of the PC) into a recording. It works out which program is the game and which address is its server, groups background apps, and keeps the numbers for a live view. |

Neither crate calls the operating system, so both build and test anywhere.

## How it decides

- **Stutters.** Every second, each possible cause is either active or not: another app in a CPU or disk burst,
  the CPU maxed out or slowed down, RAM or GPU memory full, the GPU throttling. A cause is only blamed when it is
  active far more often during stutters than the rest of the time. An app that is always a little busy never
  gets blamed for hitches it didn't cause. When nothing stands out, culprit says it was most likely the game
  itself, and uses the frame's CPU and GPU busy time to tell shader-compile style hitches from GPU-heavy frames.
- **Lag.** The router, the internet provider's first hop and the game server are pinged side by side. If the
  router is slow too, it's your home network or Wi-Fi. If only the provider's hop is slow, it's the provider. If
  both are fine, it's the server or the route to it. An app using lots of bandwidth at the same moment is checked
  first.
- **The hotkey.** Moments the player marks are matched to the nearest incident. If there is no incident nearby,
  culprit reports what was worst in the seconds before.

## Tests

```
cargo test
```

`core/tests/scenarios.rs` builds whole sessions with a known culprit and checks the verdict. The scenarios cover
Defender scans, Wi-Fi lag, provider trouble, server trouble, a server that ignores pings, a Steam download, an
overheating GPU, a clean session and hotkey marks.

## License

MIT
