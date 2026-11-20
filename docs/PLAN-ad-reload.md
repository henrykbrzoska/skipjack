# Plan 2: eksperyment „wyjdź i wróć” na reklamie niepomijalnej

Hipoteza: samo `setPlaylist` (`docs/LOUNGE_API.md` 3.1) w trakcie reklamy nic
nie daje — TV trzyma sesję reklamową. Trzeba **najpierw wyjść z odtwarzacza**
(`dpadCommand BACK`), a potem **wrócić do tego samego filmu w tym samym
miejscu** (`setPlaylist videoId=<content> currentTime=<pozycja>`).

Flaga: `experimental_reload_ads` (domyślnie `false`), `--try-reload` w CLI.

## Algorytm (`LoungeClient::try_ad_reload`)

1. Trzymamy **stan treści** (`Session::content`: `video_id` + `position`),
   aktualizowany tylko eventami treści — payloady z `adVideoId`/`adState`
   oraz stany `1081`/`1082` są ignorowane. Wcześniej pozycja była brana
   z `playback.current_time`, które reklama nadpisywała zerem (stąd
   `currentTime=0` w logu).
2. Na `adPlaying` / `onAdStateChange(adState=1)` / `state=1081`
   z `isSkipEnabled=false`:
   - jeśli tę reklamę (`adVideoId`) już próbowaliśmy — nic nie robimy
     (`onAdStateChange` nie ma id, więc echo tej samej reklamy nie startuje
     drugiej próby),
   - zapamiętujemy `contentVideoId` (albo ostatni znany film) + pozycję,
   - wysyłamy `BACK`, czekamy 1.5 s (TV musi zdjąć sesję reklamową),
   - wysyłamy `setPlaylist videoId=<film> currentTime=<pozycja>`,
   - **wysyłamy `play`** — `setPlaylist` tylko *ustawia* film (TV zostaje na
     pauzie/„cued”), więc bez tego nic się nie odtwarza (`start_playback()`,
     3 próby co 0.7 s, komenda idempotentna).
3. Kryterium sukcesu: `nowPlaying` ze `state=1` i tym samym `videoId`
   w ciągu 15 s. Porażka (inny film / za późno / błąd / utrata sesji)
   kończy eksperyment na tę sesję.
4. Limit: 1 próba na reklamę; `MAX_AD_RELOADS = 3` to budżet **kolejnych
   porażek** (sukces go zeruje), a nie limit na sesję.

## Pułapki, które już wywaliliśmy

- **Druga reklama nie była ruszana**, bo `pending` (próba w toku) blokował
  kolejne próby aż do `state=1`, a TV często raportuje powrót tylko przez
  `onStateChange`, nie `nowPlaying`. Teraz: nowa reklama (`adVideoId` inny niż
  w próbie) kończy poprzednią próbę, a echo tej samej reklamy
  (`onAdStateChange` bez id) jest ignorowane.
- **`state=1` za późno** oznaczał `aborted` na całą sesję (log
  „failure: too late”). Teraz spóźniony powrót to tylko info — budżet porażek
  liczy się dalej.
- **Pozycja 0 zamiast miejsca reklamy**: `playback.current_time` nadpisuje
  reklama, więc pozycję bierzemy ze stanu treści (`Session::content`).

## Czego pilnować

- `BACK` w trakcie reklamy zwykle wychodzi z odtwarzacza (do kolejki / na
  ekran główny), a `setPlaylist` przywraca film — ale YouTube może wtedy
  zaserwować nowy pre-roll; dlatego limit prób jest twardy.
- Po powrocie `state=1` wywołuje `ad_ended()`, więc wyciszenie reklamy
  zostaje zdjęte automatycznie.
- Jeśli metoda okaże się nie działać, alternatywą pozostaje przyspieszanie
  odtwarzania reklamy — `PLAN-ad-time.md` 5.4.
