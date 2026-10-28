# Performance

Mochune prioritizes a small memory footprint, then speed. The figures below
are short **release-build observations from 25 October 2026**, not requirements,
maximums, or promises for every machine and video. These historical measurements were collected before the Mochune rename,
using the then-current test working tree rather than a published release. [Sanitized numeric samples](assets/performance-samples.json).

## Original release-build playback samples

| Platform / state | Private bytes | Working set / RSS | PSS |
| --- | ---: | ---: | ---: |
| Windows · Music | **36.5 MiB** | 64.0 MiB working set | — |
| Windows · 720p60 Video, app + worker | **311.6 MiB** | 347.5 MiB working set | — |
| Linux · idle Video page | — | **117.0 MiB RSS** | 108.5 MiB |
| Linux · Music | — | **146.3 MiB RSS** | 137.2 MiB |
| Linux · 720p60 Video | — | **190.8 MiB RSS** | 181.2 MiB |

Windows values sum the main app and its native video worker, when present.
**Private bytes** are committed private process memory; **working set** is
resident process memory, including shared pages. Linux **RSS** includes shared
resident pages; **PSS** apportions them. These columns are deliberately separate:
Windows private bytes should not be compared directly against Linux RSS.

Transient yt-dlp/Deno helpers, the desktop compositor, dedicated/shared GPU
allocations and unrelated processes are excluded. Extraction can temporarily
raise total memory use. Longer sessions, larger feeds, themes/GIFs, codec,
resolution and driver behavior can change the figures.

Task Manager is a separate observation and uses its own memory definition and
sampling time; its displayed number need not equal private bytes or working set.

## Conditions

- **Windows:** physical Ryzen 5 3550H test laptop; NVIDIA GTX 1650 and Radeon Vega
  8 adapters present. Default appearance, real YouTube playback, isolated profile,
  no completed-audio cache. Automatic related-video fetching was disabled;
  Music still populated its normal Similar list. Native D3D12 video path. Five samples at one-second
  intervals, after playback begins; medians are shown. Main app and matching
  video-worker processes are included together. The screenshots/recording use a
  separate profile from the owner's library.
- **Linux:** Debian, kernel 6.12.107, Wayland, Intel i5-7200U / HD Graphics 620.
  Native release executable, default appearance, isolated profile, audio caching
  disabled, volume muted. Median of one-second samples between 18 and 26 seconds
  after launch in a 28-second session. Music playback and position progression
  confirmed; video verified `vah264dec` with `VAMemory`, 720p60.
- **Media:** Kevin MacLeod's *Carefree* and Blender Foundation's *Big Buck Bunny*.
  These are real network streams, not pre-decoded local fixtures.
- No recording or per-frame screenshot collection ran during the memory sample
  interval. This is a small sample, not a long-duration leak or stress test.

## Linux software-rendered Music

The README's **~85 MiB Linux Music** figure is an observed software-rendered Music sample, not the OpenGL release sample above. Music now uses software UI rendering by default; switching modes re-executes the app, preserving bounded session metadata. The normal Video renderer is unchanged.

| Local development build | RSS | Private resident | Shared resident | PSS |
| --- | ---: | ---: | ---: | ---: |
| Software Music · idle | 74.5 MiB | 60.2 MiB | 14.3 MiB | 64.8 MiB |
| Software Music · playing | **83.7 MiB** | 68.7 MiB | 15.1 MiB | 73.6 MiB |
| OpenGL Music · playing | 151.1 MiB | 138.2 MiB | 12.9 MiB | 140.8 MiB |

Measured on the Linux machine described above, using the same existing local 45-second media fixture's audio track, default appearance, a non-maximized window and an isolated empty profile. Playback progressed with no video decoder or displayed video. These are medians from 250 ms samples between seconds four and eight of nine-second sessions. No extraction helper or populated network recommendation list was involved; real network playback and themes can differ. Swap was zero. [Sanitized experiment samples](assets/linux-software-memory.json).

Private resident memory is `Private_Clean + Private_Dirty`; it includes unshared file-backed code, so it is not identical to Windows' private working set or committed Private Bytes. The software playback sample contained 52.0 MiB private clean and 16.7 MiB private dirty pages. Reporting only the latter would omit resident code and is not the metric used in the README.

Two local round trips reached the event loop after window show in 363–428 ms into Music and 242–244 ms back into Video. That measures UI readiness rather than the compositor's first visible frame. Existing hardware Video playback and restoring a selected video's resume position were also checked. These figures describe those test runs, not guaranteed launch/switch times.

## Responsiveness

A bounded Windows post-recovery test measured a **183 ms median** video seek; three Music slider seeks measured **30 ms median**. These are small historical samples, not a promise for every seek or the first audible/displayed sample. Seeking outside retained compressed data requires fetching and decoding again. Network conditions, keyframes and YouTube resolution all matter.

## What keeps it light

- A native UI instead of a bundled browser engine.
- Audio-only Music playback; its video decoder/GPU presenter is not created.
- Windows video rendering lives in a separate, on-demand Mochune worker. Leaving
  Video releases it; the worker also exits after an idle delay at playback end.
- Hardware-only video decoding, with a clear error if the required hardware path
  is unavailable. Audio decoding is independent of that policy.
- Compressed playback buffers have bounded budgets. No speculative stream
  prefetch across the whole recommendation list.
- Virtualized/reused list rows and bounded, viewport-driven thumbnail loading.
- Theme artwork/font budgets and visibility-gated animation work.
- Playback is prioritized over queued browsing and recommendation work.

## Download size

The application executable and the complete package are different measurements.
The current locally produced Windows installer and portable download are each
about **99 MiB compressed**, including playback/helper dependencies; the Debian
package is about **79 MiB**. Personal data and optional caches add disk use.
These are local artifacts, not published release sizes.
