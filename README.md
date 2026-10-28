<p align="center"><img src="ui/assets/logo-128.png?v=mochune-20261028" width="80" alt="Mochune"></p>
<h1 align="center">Mochune</h1>
<p align="center"><strong>A low-ram and fast yt-dlp wrapper for listening to music and playing videos. Built with Rust and Slint using AI.</strong></p>
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

## Music with your own look

A lightweight Music player you can theme: colors, fonts, images and animated GIFs.

![Music themes: Phosphor CRT, PastelDuck, then PixelDuck](docs/assets/themes-carousel.gif?v=mochune-20261028)

**Phosphor CRT → PastelDuck → PixelDuck** — all included. Open the themes folder from Settings to customize yours. [Make a theme →](themes/AUTHORING.md)

### Compact Music player

Keep just the player, or shrink it to the mini controls.

<table>
  <tr><th>Phosphor CRT</th><th>PastelDuck</th></tr>
  <tr>
    <td align="center" valign="top">
      <img src="docs/assets/music-crt-compact.png?v=mochune-20261028" width="300" alt="Compact Phosphor CRT Music player">
      <br><br>
      <img src="docs/assets/music-crt-mini.png?v=mochune-20261028" width="332" alt="Mini Phosphor CRT Music player">
    </td>
    <td align="center" valign="top">
      <img src="docs/assets/music-pastel-compact.png?v=mochune-20261028" width="300" alt="Compact PastelDuck Music player">
      <br><br>
      <img src="docs/assets/music-pastel-mini.png?v=mochune-20261028" width="332" alt="Mini PastelDuck Music player">
    </td>
  </tr>
</table>

## Try Mochune

**Windows x64:** installer or portable app. **Debian x64:** `.deb` package.

**[Download the latest alpha](https://github.com/Quaint69/Mochune/releases/latest).** Bug reports are welcome. [Build from source](docs/DEVELOPMENT.md) · [Packaging](packaging/README.md) · [User guide](docs/USAGE.md)

<sub>Independent project; unaffiliated with YouTube or Google. Availability depends on YouTube. Media rights remain with their owners.</sub>

[GPL-3.0-only](LICENSE) · [Third-party notices](NOTICE) · [Media credits](docs/assets/README.md)

<p align="center"><strong>Support Mochune:</strong> <a href="https://ko-fi.com/duckeytubey">☕ Ko-fi</a> · <a href="https://www.paypal.com/ncp/payment/Y3ND4S8E29RS4">PayPal</a></p>
