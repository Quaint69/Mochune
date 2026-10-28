# Using Mochune

## Video and Music

The header switches between independent Video and audio-only Music modes. The last mode is remembered. Select a result to play; browsing Home, a channel or a playlist does not start playback by itself.

Search accepts words, YouTube links and `@channel` searches. Channel results are included for `@` queries. Home searches show a dedicated results grid; searches from the player use its adjacent list.

Video offers quality/frame-rate selection, captions, a playback queue and comments loaded on demand. The arrows in the controls move them inside or below the picture; fullscreen keeps them inside. Mouse movement reveals hidden controls and the pointer. The description supports selection and copying selected text.

Music offers For You, Similar and From Likes suggestions, separate playlists, synchronized captions/lyrics and a compact player. Selecting a search result carries that song to the top of Similar, rather than continuing through the search results. Lyrics depend on the captions supplied for that video; language and availability vary. Repeat cycles through Off, Repeat list and Repeat song. Choosing a playlist changes the next-song list without interrupting the current track. Liked songs are permanent and ordered newest first.

## Recommendations and history

Video Home starts empty until there is local viewing data. New refreshes suggestions; History shows actual played videos. Related uses the current video as its source. Music recommendations apply song-duration and other eligibility filters. From Likes uses at most ten rotating/spread seeds per request, rather than fetching every liked song at once.

Music and Video histories are independent. The current file is `history-v2.json`. The old shared history is preserved as `history-legacy.json.bak` rather than silently treating old mixed records as one mode's history. Settings exposes recommendation limits, exclusions and minimum views. Recommendations are not a signed-in YouTube account feed.

## Playlists and backups

Music and Video keep separate libraries. Video playlist selection displays its saved videos; queue actions are separate. Settings supports Music, Video or combined import/export. JSON retains library metadata; M3U8 exchanges ordered links. JSON import merges matching playlist identities without duplicating existing tracks; unknown metadata is retained. The previous good library is kept in a `.bak` file. Close the app and preserve both files before attempting manual recovery. Export a backup before uninstalling.

## Downloads and cached Music

Downloads are explicit exports. Cached Music is separate: its page controls optional saving of completed compressed audio for offline listening. Incomplete tracks are not presented as ready offline files. Storage settings separate cached audio, thumbnails and temporary stream-link metadata; clearing a cache deletes that category's stored data.

Stream-link caching is on by default and retains a few recent format responses temporarily, not video files. Thumbnail disk caching is optional. Playback RAM buffers are bounded and disposable; a gray buffered range is not a permanent download or a guarantee that seeking needs no keyframe decoding.

Use downloads and offline caching only with the necessary content permissions or applicable rights. Mochune is independent of YouTube.

## Themes and data locations

Music supports the default appearance and bundled PastelDuck, PixelDuck and Phosphor CRT themes. Settings → Themes opens the active themes folder. Edit a theme's `skin.json`, rescan folders after additions, and reload after edits. See [theme authoring](../themes/AUTHORING.md) for supported properties, fonts and resource limits.

The Windows portable launcher keeps its profile in `Mochune/data` beside the launcher; replacing its runtime preserves that profile. The installer uses `%LOCALAPPDATA%/Mochune` for app data and allows a per-user install location. Linux uses its normal user data/config directories. Development runs may override the data directory.

Installed-app uninstall removes standard Mochune profiles, including custom themes. Exported media downloads and separate portable/development profiles are separate. Close the app before updating, uninstalling or manually recovering library files.

## Playback requirements

Video requires a supported hardware decoder and graphics driver; software video decoding is disabled. Audio decoding is independent. Windows uses a native D3D12 video worker, while Linux uses the available Wayland/X11 graphics path. Missing codec/driver support, expired links, network failures and YouTube restrictions can prevent a particular stream from playing. Playback errors should be reported with platform, video, selected quality and the relevant logs; do not publish cookies or signed stream URLs.
