# TrackMap — plan

TrackMap is a Rekordbox alternative for organising music for the nap.works **Devious Pocket**.
Written in Rust with GPUI. Graph-based UI where songs, artists, labels and playlists are nodes.

## What the player expects (facts we design around)

From docs.nap.works and the Pocket OS 1.2 release email:

- Music comes from a **USB drive** (FAT32, exFAT, EXT4 or NTFS).
- Formats: mp3, flac, wav, aif/aiff, caf, ogg. **No AAC.**
- Browsing is folder-based. It sorts by title/artist/album/BPM/key/genre from **ID3 tags**.
- Playlists = **Rekordbox playlist `.txt` exports** placed "any place on your USB", on a
  "rekordbox-formatted USB". Tracks are matched against files in the **`Contents`** directory.
  Missing files show greyed out. Playlist BPM/Key come from the `.txt`, not the file.
- FAT32/exFAT cannot hold symlinks, so symlinked playlist folders are not usable on most sticks.

**Open item:** the exact `.txt` format (columns, delimiter, encoding) and the matching rule are
undocumented. Get a real Rekordbox export (the `.txt` plus `find /Volumes/STICK -maxdepth 4`) and build
the exporter plus a golden-file test from it. Current belief (unverified): tab-separated, UTF-16 LE
with a BOM, header row `#  Track Title  Artist  …  BPM  Key …`, and files at
`/Contents/<Artist>/<Album>/<file>`.

## Model

- **Library root**: a folder on the Mac, organised any way the user likes. It is watched for changes.
- **`<root>/Playlists/`**: every subfolder is a playlist, and its entries are **symlinks** to library
  files (APFS supports them). Folders created in Finder become playlists and vice versa. The scanner
  skips `Playlists/` so tracks aren't counted twice.
- **`<root>/.trackmap/library.db`**: SQLite cache of tags, analysis and waveform peaks, keyed by
  path + content hash so moves and renames keep their data.
- **Sync to USB**: writes the layout the player expects.
  ```
  USB/
    Contents/<Artist>/<Album>/<file>   each track copied once, incremental
    Playlists/<name>.txt               Rekordbox-format playlist, incl. BPM + Key
  ```

## Architecture

```
crates/
  core/    library model, scan, watch, tags, analysis, playlists, USB sync (no UI)
  graph/   node/edge model, filtering, layout (no UI)
  app/     GPUI shell: canvas, top bar, inspector, preview bar, ⌘P palette
```

| Concern | Crate / approach |
|---|---|
| Watching | `notify` + `notify-debouncer-full`, plus a reconciliation scan on startup |
| Tags | `lofty`. Read title/artist/album/label (`TPUB`)/genre/BPM (`TBPM`)/key (`TKEY`) |
| Decoding | `symphonia` |
| BPM / key | Use tags when present. Otherwise onset+autocorrelation BPM and chromagram key (or libKeyFinder FFI). Store keys as Camelot |
| Tag write-back | Write detected BPM/key back to tags (setting, on by default) so the Pocket can sort on them |
| Index | `rusqlite` |
| Search | `nucleo` fuzzy matching |
| Hit-testing | `rstar` spatial index |
| Preview audio | `rodio` (on `cpal` + `symphonia`) on a dedicated audio thread, controlled through a channel |

## Map

**Only songs are drawn.** Artists, labels and playlists are groups of songs. Selecting one
highlights its songs and dims the rest; they never appear as nodes.

**Layout (tempo first)**
- x = BPM, slow left to fast right. The axis is fitted to the library's tempo range (rounded to
  5 BPM) and labelled along the bottom. Songs without a BPM get a column on the far left.
- y = key, secondary: Camelot rows with A/B pairs adjacent (1A, 1B, 2A, …), so relative
  major/minor and neighbouring keys stay close. Songs without a key get a row at the bottom.
- A small stable jitter keeps songs with the same key and BPM from stacking.
- Songs are *not* grouped by artist/label/playlist in space; position is purely musical.

**Selecting**
- Click a song: it's highlighted, and the inspector lists its artist, label and playlists as links.
- Artists ▾ / Labels ▾ / Playlists ▾ chips open a list; picking one highlights its songs.
- A selected label lists its artists in the inspector (label → artist → songs).
- A selected playlist also draws its route through the map in play order, numbered.
- Filters (New, later search/BPM/key) limit which songs are shown; a selection's songs always show.
- Esc closes the picker, then clears the selection.

**Interaction (planned)**
- Drag a song onto a playlist (drop targets appear while dragging), plus "+ New playlist".
- Box-select (shift-drag) for several songs.
- Right-click: reveal in Finder, remove from playlist.

## UI layout (no sidebar)

```
┌───────────────────────────────────────────────────────────┬─────────────┐
│ ~/Music ▾ [Search…]  BPM ▭▭  Key ▾  New  Playlists  Labels  Sync ⇪●     │
├───────────────────────────────────────────────────────────┬─────────────┤
│                                                           │ Inspector   │
│                    graph canvas                           │ (only when  │
│                                                           │  selected)  │
├───────────────────────────────────────────────────────────┴─────────────┤
│ ▶  ▁▃▅▇▅▃▁▂▄▆▇▅▃   1:23 / 6:40                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

- **⌘P** (as in VS Code): fuzzy quick-open across songs, artists, labels and playlists. A leading
  `>` switches to commands. **⌘⇧P** opens directly in command mode (new playlist, sync, open folder, …).
- The sync button has a status dot: orange means there are unsynced changes.

## Preview

- **Space** plays/pauses the selection. Clicking another song switches instantly.
- Playback starts at 30% (configurable) so you skip the intro. A key cycles 25/50/75%.
- Waveform in the inspector and preview bar, click to seek. Peaks are computed in the analysis pass
  and cached.
- The playing node pulses on the canvas.
- **Arrow keys** jump to the nearest harmonic neighbour (compatible key, closest BPM) and preview it.
- Output device picker (`cpal`) for headphones on a multi-output interface.

## Theme (Postman-like)

Dark neutral greys with one orange accent, compact. One `Theme` global in `crates/app/src/theme.rs`.

| Token | Value |
|---|---|
| `bg_app` | `#1C1C1C` |
| `bg_panel` | `#212121` |
| `bg_elevated` | `#2B2B2B` |
| `border` | `#363636` |
| `text` / `text_muted` | `#E6E6E6` / `#9B9B9B` |
| `accent` / `accent_hover` | `#FF6C37` / `#FF8A5C` |

Nodes are muted by default. Each song has a ring in its Camelot key colour. Selected or playing
nodes are orange, with their edges orange and everything else dimmed to about 30%.

## Milestones

0. **Device check:** get a real Rekordbox `.txt` export and the stick layout. Optionally test whether
   the Pocket follows symlinks on EXT4 (not required).
1. **Core:** open a folder, scan, read tags, SQLite cache. CLI: `trackmap scan <dir>`.
2. **Watcher:** debounced events kept in sync with the DB, plus a startup reconciliation scan.
3. **Playlists:** symlink folders in `Playlists/` (create/rename/delete, Finder edits reflected).
4. **GPUI shell:** theme, top bar, filters, ⌘P palette, inspector, preview playback.
5. **Graph canvas:** harmonic layout, pan/zoom, click-to-expand, edges, drag-to-playlist.
6. **Analysis:** BPM/key detection, tag write-back, waveform peaks, progress UI.
7. **USB sync:** `Contents/` copy plus `.txt` export, golden-file tested against the real sample.
