# Plan 1: pełne sterowanie YouTube przez Lounge API

Stan na dziś: `LoungeClient` (`skipjack/crates/skipjack-core/src/lounge.rs`) umie
`setVolume/mute`, `skipAd`, `setPlaylist(videoId)`, `getNowPlaying`, `setAutoplayMode`
**oraz cały transport z 1.1** (`play/pause/seekTo/next/previous/setPlaybackSpeed/
setSubtitlesTrack/dpadCommand`, podpięte do `Service`, CLI `remote` i komend Tauri + pilot w GUI).
Reszta komend z `docs/LOUNGE_API.md` (rozdz. 3) jest w 1.2–1.3.

## 1.1 Transport ✅ (20.11.2026)

Dodać do `LoungeClient` (wszystko przez istniejące `command()`, koszt mały):

| Metoda | Komenda | Parametry |
|---|---|---|
| `play()` / `pause()` | `play` / `pause` | – |
| `seek_to(secs)` | `seekTo` | `newTime` |
| `next()` / `previous()` | `next` / `previous` | – |
| `set_speed(x)` | `setPlaybackSpeed` | `playbackSpeed` |
| `set_subtitles(video_id, lang)` | `setSubtitlesTrack` | `videoId`, `languageCode` |
| `dpad(key)` | `dpadCommand` | `key` = `UP/DOWN/LEFT/RIGHT/ENTER/BACK` |

Podpięcie: `Service::remote_*` (rejestr pilotów per `screen_id`), CLI
`skipjack remote <device> <play|pause|seek|next|previous|speed|subtitles|dpad|play-video>`
(one-shot: auth → connect → komenda → disconnect), komendy Tauri `remote_*`
+ sekcja pilota na dashboardzie. `DpadKey` w `skipjack-core::lib`.

## 1.2 Kolejka ✅ (20.11.2026)

- `setPlaylist` pełne: nowy `Playlist { video_id, video_ids, current_index,
  current_time, list_id }` → `set_playlist()` (stare `play_video()` woła je
  z samym `videoId`, kompatybilne wstecz).
- Nowe: `add_video`, `insert_video` (za aktualnie granym), `remove_video`,
  `clear_playlist` + `add_videos()` (jeden film na request, pauza 2–5 s
  z `thread_rng` dropniętym przed `sleep`, żeby future był `Send`).
- Rebind-awaryjny: `queue_command()` — gdy sesja padnie (400/410/401),
  re-auth + re-bind i jeden retry komendy.
- Podpięcie: `Service::remote_{set_playlist,add_video,add_videos,insert_video,
  remove_video,clear_playlist}`, CLI `remote <set-playlist|add-video|add-videos|
  insert-video|remove-video|clear-playlist>`, komendy Tauri `remote_*` + wiersz
  kolejki w pilocie na dashboardzie (input akceptuje id lub URL, Play/Clear
  z `confirm()` bo przerywają oglądanie).

## 1.3 Odczyt stanu ✅ (20.11.2026)

- `playlistModified` → `QueueState { list_id, video_ids }` trzymany w `Session`,
  emitowany jako `LoungeEvent::Queue` (też z pierwszej ramki binda — migawka
  kolejki z momentu otwarcia sesji).
- `onVolumeChanged` → `VolumeState { volume, muted }` (emit), `onVideoQualityChanged`
  → `QualityState { level, available }` (emit + trzymany w sesji).
- `watch_queue_ajax` (`GET /watch_queue_ajax?action_get_watch_queue_items=1&list=…`,
  nagłówek `X-YouTube-LoungeId-Token`) → `watch_queue()` + parser
  `<li class="…watch-queue-item" data-video-id>` z tytułem (`title=` lub tekst,
  z `html_unescape`).
- Forward: `ServiceEvent::{Queue,Volume,Quality}` → CLI loguje (kolejka/głośność/
  jakość), `Service::{queue,queue_items,volume,quality}` → komendy Tauri
  `remote_{queue,queue_items,volume,quality}` → UI trzyma w `DeviceLive`
  i pokazuje pod pilotem (⧉ N w kolejce, 🔊 vol, Xp).
- CLI `remote <device> queue`: migawka z binda + tytuły przez `watch_queue_ajax`
  (fallback do samych id, gdy brak `listId` lub błąd).

## Kolejność i bezpieczeństwo

1. Transport (1 wieczór testów na TV).
2. Kolejka (z rebind-awaryjnym).
3. Odczyt kolejki.

`next/previous/setPlaylist` przerywają oglądanie — w UI dać potwierdzenie
+ opcjonalny tryb „tylko gdy pauza”.
