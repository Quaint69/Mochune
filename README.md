<p align="center"><img src="ui/assets/logo-128.png?v=mochune-20261028" width="80" alt="Mochune"></p>
<h1 align="center">Mochune</h1>
<p align="center"><strong>A low-RAM, fast app for YouTube music and videos, anime and movie streaming, and reading manga. Built with Rust and Slint using AI.</strong></p>
<p align="center">
  <a href="LICENSE"><img alt="GPL v3" src="https://img.shields.io/badge/license-GPLv3-F4A62A?style=flat-square"></a>
  <img alt="Windows and Linux" src="https://img.shields.io/badge/platforms-Windows%20%7C%20Linux-35302A?style=flat-square">
  <img alt="Alpha" src="https://img.shields.io/badge/status-alpha-35302A?style=flat-square">
</p>
<p align="center"><a href="#light-and-fast">RAM &amp; Performance</a> · <a href="#music-with-your-own-look">Themes</a> · <a href="#compact-music-player">Compact player</a> · <a href="docs/DEVELOPMENT.md">Build</a></p>

<p align="center">
  <img src="docs/assets/music-full.png?v=mochune-20261028" width="75.889%" alt="Full Mochune Music interface">
  &nbsp;&nbsp;
  <img src="docs/assets/music-default-compact.png?v=mochune-20261028" width="21.111%" alt="Compact Mochune Music player">
</p>

## Light and fast

Optimized to launch quickly, keep playback and seeking responsive, and switch smoothly between Music and Video. Playback resources are released when you leave Video or close the app. Rust and Slint keep the interface native, without a bundled browser engine.

| Playing | Windows · private memory | Linux · RAM (RSS) |
| --- | ---: | ---: |
| Music | ~37 MiB | ~85 MiB* |
| Video · 720p60 | ~312 MiB | ~191 MiB |

*Linux Music uses software UI rendering by default, measured with local audio playback in a development build. Mode changes relaunch the app.

Observed samples, not limits. Windows and Linux columns use different memory metrics; video includes the Windows worker. [Measurements →](docs/PERFORMANCE.md)

## Listen, watch and read

Choose **YT Music · YT Video · Anime · Manga · Movie** from the mode picker.

- **Music:** playlists, likes, synced lyrics, shuffle/repeat, and compact or mini players.
- **YouTube video:** a Home feed, channel pages, quality selection, captions, queues and comments loaded only when requested.
- **Audio effects:** optional one-sided channel delay (0–20 ms) and an impulse-response convolver. Use the included `.irs`, select your own file, or download the optional collection.
- **Experimental interpolation:** 2× or 60-fps GPU frame generation on Linux and Windows, for supported 12–30-fps sources at 720p or lower. Off by default, with automatic fallback when the GPU cannot sustain it. Motion artifacts can occur.

## Music with your own look

Theme Music and Video with colors, borders, rounded corners, fonts, images and animated GIFs. Layout stays managed by the app.

![Music themes: Mochune CRT, Mochune Pastel, then Mochune Pixel](docs/assets/themes-carousel.gif?v=mochune-20261028)

**Mochune CRT → Mochune Pastel → Mochune Pixel** — all included. Open the themes folder from Settings to customize yours. [Make a theme →](themes/AUTHORING.md)

### Compact Music player

Keep just the player, or shrink it to the mini controls.

<table>
  <tr><th>Mochune CRT</th><th>Mochune Pastel</th></tr>
  <tr>
    <td align="center" valign="top">
      <img src="docs/assets/music-crt-compact.png?v=mochune-20261028" width="300" alt="Compact Mochune CRT Music player">
      <br><br>
      <img src="docs/assets/music-crt-mini.png?v=mochune-20261028" width="332" alt="Mini Mochune CRT Music player">
    </td>
    <td align="center" valign="top">
      <img src="docs/assets/music-pastel-compact.png?v=mochune-20261028" width="300" alt="Compact Mochune Pastel Music player">
      <br><br>
      <img src="docs/assets/music-pastel-mini.png?v=mochune-20261028" width="332" alt="Mini Mochune Pastel Music player">
    </td>
  </tr>
</table>

## More sources

Use the mode picker to switch between YouTube and the other sources:

- **Anime:** title search, Sub/Dub episodes, available source qualities and captions, and automatic next episode.
- **Movies:** search titles through Dramachi or VixSrc. VixSrc uses [Wikidata](https://www.wikidata.org/) title metadata and also accepts TMDB movie links/IDs.
- **Manga:** MangaDex and MangaPill, scrolling/page modes, fullscreen, fit-width reading and Ctrl+wheel zoom.

Organize anime, movie and manga bookmarks in named folders, with saved manga reading progress.
These views follow the selected theme. Manga keeps a small decoded viewport and bounded compressed lookahead;
source playback stays separate from YouTube history and recommendations.
Video decoding requires supported hardware; there is no CPU video-decoding fallback.
Provider availability can change. Switching providers is explicit.

## Try Mochune

**Windows x64:** installer or portable app. **Debian x64:** `.deb` package.

**[Download the latest alpha](https://github.com/Quaint69/Mochune/releases/latest).** Bug reports are welcome. [Build from source](docs/DEVELOPMENT.md) · [Packaging](packaging/README.md) · [User guide](docs/USAGE.md)

<sub>Independent project; unaffiliated with YouTube or Google. Availability depends on YouTube. Media rights remain with their owners.</sub>

[GPL-3.0-only](LICENSE) · [Third-party notices](NOTICE) · [Media credits](docs/assets/README.md)

<p align="center"><strong>Support Mochune:</strong> <a href="https://ko-fi.com/duckeytubey">☕ Ko-fi</a> · <a href="https://www.paypal.com/ncp/payment/Y3ND4S8E29RS4">PayPal</a></p>
