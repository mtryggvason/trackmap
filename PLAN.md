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
- **Sync to USB** (done): writes the layout the player expects, onto an existing Rekordbox USB
  without disturbing it. A manifest on the drive (`.trackmap-sync.json`) records every file
  TrackMap wrote; only those are ever updated or removed. Identical files already there are
  reused; different files in the way are never touched (the track gets another name). Atomic
  copies, resumable, FAT32/exFAT-safe names, no `._` files, AAC skipped. Preview before
  copying; whole library or playlist tracks only; removing deleted tracks is opt-in.
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

**Layout (tempo × key)**
- x = BPM. Each occupied tempo bin gets one fixed-width slot, so neighbouring tempos keep their
  true spacing while empty stretches are compressed to a short break (marked ⋯ on the axis).
  Labels show the tempos that have songs.
- Tempos are grouped by whole BPM at every zoom level; zooming only magnifies (covers and
  gaps grow), so nothing moves under your fingers. Pinch has momentum and rubber-band limits.
- y = key, in Camelot order with A/B pairs adjacent (1A, 1B, 2A, …), unknown keys at the bottom.
  Subsets only show the keys they contain.
- The camera always frames the shown songs (at 1× zoom), easing when the set changes; covers
  size themselves to the space each song has (up to 160 px), so three songs are big and a whole
  library is small.
- Titles sit centred under their covers and never overlap.

**Selecting**
- Picking an artist, label or playlist narrows the map to its songs: everything else disappears
  and both scales refit to the group (its BPM range, only the keys it uses). The previous view
  is saved; Esc deselects a song inside the group first, then leaves the group and restores it.
- Click a song: it's highlighted, and the inspector lists its artist, label and playlists as links.
- Artists ▾ / Albums ▾ / Labels ▾ / Playlists ▾ chips open a list; picking one narrows the map to its songs.
- A selected label lists its artists in the inspector (label → artist → songs).
- A selected playlist also draws its route through the map in play order, numbered.
- Filters (New, later search/BPM/key) limit which songs are shown; a selection's songs always show.
- Esc closes the picker, then clears the selection.

**Playlists (done, first version)**
- Playlists ▾ → "+ New playlist…" (adds the selected song, if any). A song's inspector has
  "Add to playlist" with ✓ toggles and "New playlist…".
- A playlist's inspector lists its songs in play order with ↑ ↓ to reorder and × to remove,
  plus Rename and Delete (click twice). Only links change; music files are never touched.

**Interaction (planned)**
- Drag a song onto a playlist (drop targets appear while dragging).
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

- **Search** (done): **⌘P** opens a VS Code-style Quick Open panel at the top centre: type to
  search songs, artists, albums, labels and playlists (every word must match, any order,
  accents ignored); the map previews matches while it's open. Enter opens a result (song
  selected, group narrows the map); the first row, "Show N matching songs on the map", keeps
  the query as a map filter. Esc / clicking outside closes and keeps the search on the map
  (✕ in the top bar, or emptying the field, clears it). The top-bar
  box works like VS Code's command centre: it opens Quick Open and shows an active filter
  with ✕. Each track's search text is prepared once, so a keystroke scans 20,000 tracks in
  under 1 ms.
- Later: a `>` command mode for actions (new playlist, sync, open folder, …).
- The sync button has a status dot: orange means there are unsynced changes.

## Preview

Done (first version):
- Click a song, then **Space** to play/pause it (or ▶ Play in the inspector). Space resumes or
  restarts what's loaded when no other song is selected.
- **← / →** jump back/forward 10 s.
- Preview bar: play/pause, title and artist, elapsed / total, and the track's waveform (played
  part in orange, click to seek). Waveforms are computed in the background on first play and
  cached in `.trackmap/waves/`.
- The playing song has an orange ring on the map.
- Decoding reuses the analysis decoder (symphonia) as a seekable stream; rodio only does output.

Next:
- Start position option (e.g. 30%) to skip intros.
- Arrow keys to jump to the nearest compatible track (key ±1, close BPM) and preview it.
- Output device picker (cpal) for headphones on a multi-output interface.

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
