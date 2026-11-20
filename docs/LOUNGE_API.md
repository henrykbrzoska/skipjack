# YouTube Lounge API – opis

Nieoficjalne, nieudokumentowane API, którego używa „Połącz z TV kodem” w apce
YouTube na telefonie. Telefon (albo Skipjack) dołącza do „lounge” telewizora jako
**pilot** (`REMOTE_CONTROL`), dostaje zdarzenia z odtwarzacza i może wysyłać komendy.

Źródła tego opisu:

- **[LOG]**: potwierdzone w `~/.local/share/skipjack/logs/skipjack.log` (TCL BeyondTV2,
  Android 9, klient `TVHTML5` 7.20261118, 20.11.2026);
- **[PY]**: z kodu klienta lounge w Pythonie, nie testowane na Twoim TV;
- **[NET]**: z niezależnych implementacji znalezionych w sieci (ytcast, ytlounge,
  youtube-cast-automation-api, youtube-remote, casttube, plaincast), nie testowane
  na Twoim TV;
- **[?]**: hipoteza, nie sprawdzona.

Baza: `https://www.youtube.com/api/lounge`

---

## 1. Endpointy

### 1.1 `POST /pairing/get_screen`: parowanie kodem [PY]/[NET]
Body (form): `pairing_code=123456789012` (12 cyfr z TV, bez myślników).
`youtube-remote` wysyła to samo jako GET z query stringiem [NET].

Zwraca:
```json
{"screen": {"screenId": "…", "name": "YouTube on TV", "loungeToken": "…", "expiration": …}}
```
`screenId` to **stały identyfikator TV** i to on trafia do `config.json`. Kod parowania
jest jednorazowy i krótko ważny. W odpowiedzi jest też `deviceId` i `name` ekranu [NET].
`expiration` w tej odpowiedzi to **string**, na endpointcie tokenowym – liczba [NET].

### 1.2 `POST /pairing/get_lounge_token_batch`: token sesji [LOG]
Body: `screen_ids=<id1>,<id2>,…` (wiele naraz).

Zwraca:
```json
{"screens": [{"screenId": "…", "loungeToken": "…", "expiration": 1763…}]}
```
Token wygasa (czas ważności jest w polu `expiration` [?]; Skipjack odświeża go co 24 h i po każdym `401`).
Według `desvaters/ytlounge` (zweryfikowane na drucie we wrześniu 2026) token żyje
**ok. 13 dni**, a odświeżenie **nie unieważnia** wcześniejszych tokenów [NET].
`ytcast` nie odświeża wcale – pobiera token raz przy starcie [NET].
**Do pobrania tokenu wystarczy samo `screenId`**, bez logowania na konto Google.

### 1.3 `POST /pairing/get_screen_availability`: czy TV jest online [LOG]
Body: `lounge_token=<token>`.
Zwraca `{"screens":[{"loungeToken":"…","status":"online"}]}`.
W logu: `unknown` (10×), gdy apka YouTube na TV była zamknięta, i `online` (8×), gdy była otwarta.

### 1.4 `POST /bc/bind` (bez SID): otwarcie sesji [LOG]
Protokół Google **BrowserChannel** (stary mechanizm long-polling).
Ważne parametry: `loungeIdToken`, `device=REMOTE_CONTROL`, `name=<nazwa widoczna na TV>`,
`id=<stałe id pilota>`, `app=ytios-phone-20.15.1`, `VER=8`, `RID=1`, `capabilities=que,dsdtr,atp,vsp`.

Inne implementacje używają innych wartości i też działa [NET]:
`app=youtube-desktop` (ytcast, youtube-cast-automation-api), `app=android-phone-13.14.55`
(casttube), `id=remote` (mutantmonkey) albo losowy UUID (youtube-cast-automation-api).
`youtube-cast-automation-api` dokleja też `mdx-version=3`, `ui=1`, `v=2`, `zx=<losowe 12 znaków>`.
Wniosek: serwer akceptuje różne `app`/`id`, więc to pole do eksperymentów
(np. czy inny `app` zmienia zachowanie reklam albo ekranu wyboru konta) [?].

Odpowiedź to ramki „długość + JSON”:
```
1495
[[0,["c","F9B037A9B5F6B352","",8]]          ← SID sesji
,[1,["S","63pUC6qi8G0T…"]]                    ← gsessionid
,[2,["loungeStatus",{…, "devices":"[…]"}]]    ← kto jest w lounge'u + info o TV
,[3,["nowPlaying",{…}]] …
```
Kody odpowiedzi:
- `200`: OK;
- `401` + „Connection denied”: TV odrzuca pilota (obejście opisane w 3.4);
- `401` (inne): token wygasł;
- `400`/`410`: nieznany albo zamknięty SID.

### 1.5 `GET /bc/bind?RID=rpc&SID=…&gsessionid=…&AID=<ostatnie id>&CI=0&TYPE=xmlhttp`: strumień zdarzeń [LOG]
Long-poll, który trzyma połączenie i dosyła ramki. Co ok. 30 s przychodzi `noop`.
`AID` to numer ostatniego odebranego zdarzenia, więc po ponownym połączeniu nic nie ginie.

### 1.6 `POST /bc/bind?SID=…&RID=<n>`: komenda [LOG]
Body (form):
```
count=1
ofs=<licznik komend od 0/1>
req0__sc=<nazwa komendy>
req0_<param>=<wartość>
```
Odpowiedź `200` z krótkim ciałem (w logu `8`, `9`, `10`…). To tylko potwierdzenie
przyjęcia komendy, a nie jej wykonania. Faktyczny skutek widać w następnym
zdarzeniu (np. `onVolumeChanged`).

Warianty transportu [NET]:
- token można też wysyłać w nagłówku `X-YouTube-LoungeId-Token` zamiast w query (mutantmonkey, casttube);
- `Origin: https://www.youtube.com` w nagłówkach „nie szkodzi” (ytcast, casttube);
- User-Agent nie ma znaczenia: działa i `Mozilla/5.0…Chrome/96…` (ytcast), i `YouTubeRemote` (mutantmonkey).

### 1.7 `POST /bc/bind … TYPE=terminate`: rozłączenie [PY]
Zamyka sesję pilota (`clientDisconnectReason=…DISCONNECTED_BY_USER`).

### 1.8 Wykrywanie w sieci lokalnej (bez Lounge API) [PY]
- **DIAL/SSDP**: `M-SEARCH urn:dial-multiscreen-org:service:dial:1`, potem
  `GET <Application-URL>/YouTube` → XML z `<screenId>`. Tryb aktywny może
  najpierw uruchomić apkę YouTube na TV (`POST …/YouTube`).
- **Chromecast**: mDNS `_googlecast._tcp`.

---

## 2. Zdarzenia z TV: co jest w logu

| Zdarzenie | Ile | Przykład / znaczenie |
|---|---|---|
| `nowPlaying` | 45 | `videoId`, `currentTime`, `duration`, `state`, `listId`, `cpn`, `loadedTime`, `seekable*`; podczas reklamy także `adState`, `adVideoId`, a `duration` to długość reklamy; `mdxExpandedReceiverVideoIdList` to kolejka/rekomendacje |
| `onAutoplayModeChanged` | 33 | **`UNSUPPORTED` na Twoim TV**, więc opcja autoplay nic tu nie zmienia |
| `onAudioTrackChanged` | 28 | `audioTrackId`, `videoId` |
| `onHasPreviousNextChanged` | 27 | `hasNext`, `hasPrevious` |
| `onStateChange` | 24 | `state`, `currentTime`, `duration`, `loadedTime`, `cpn` |
| `playlistModified` | 19 | `listId` (zmiana kolejki) |
| `onVideoQualityChanged` | 18 | `qualityLevel` (np. 480), `availableQualityLevels` `[0,1080,720,…]` |
| `autoplayUpNext` | 17 | następny film (u Ciebie zwykle pusty) |
| `onAudioTrackListChanged` | 16 | lista ścieżek dźwiękowych (np. dubbing AI: angielski, arabski, …) |
| `onVolumeChanged` | 12 | `volume`, `muted`; potwierdza skutek `setVolume` |
| `onSubtitlesTrackChanged` | 8 | `videoId`, styl napisów |
| `onPlaylistModeChanged` | 8 | `loopEnabled`, `shuffleEnabled` |
| `onPartyGamesModeChanged` | 8 | `isActive` |
| `loungeStatus` | 8 | urządzenia w lounge'u: TV `brand=TCL model=BeyondTV2 year=2021 os=Android 9 chipset=rtd285o`, `capabilities=…ads,sads…` |
| `onAdStateChange` | 4 | `adState` `1` (start) / `0` (koniec), **zawsze `currentTime:"0"`** |
| `adPlaying` | 2 | pełne dane reklamy (niżej) |

The lounge also reports `loungeScreenDisconnected` (TV kicks the remote, e.g. on
Shorts) and `onPlaybackSpeedChanged`. They were absent from this log.

### Stany odtwarzacza (`state`) [LOG]

| Kod | Znaczenie | Ile w logu |
|---|---|---|
| `-1` | brak / zatrzymany | 2 |
| `0` | zakończony | – |
| `1` | odtwarzanie | 21 |
| `2` | pauza | – |
| `3` | buforowanie / start | 15 |
| `5` | cued (załadowany, nie gra) | 5 |
| `1081` | **reklama** | 9 |
| `1082` | reklama pominięta [PY] | – |

### Anatomia reklamy (z logu, 16:06) [LOG]
```
onAdStateChange {"adState":"1","currentTime":"0","isSkipEnabled":"false"}
adPlaying {
  "adVideoId":"-k3gN1TdXa0",      ← reklama to zwykły film na YouTube
  "contentVideoId":"azwz6WRaAbo", ← to, co oglądasz
  "duration":"30.021",
  "isSkipEnabled":"false", "isSkippable":"false", "isBumper":"false",
  "adSystem":"0", "clickThroughUrl":"", "adNextParams":"…", "remoteSlotsData":"…"
}
nowPlaying {"state":"1081","adState":"1","adVideoId":"-k3gN1TdXa0","videoId":"azwz6WRaAbo",
            "currentTime":"1.348","duration":"30.021", …}
… (co ~1 s nowPlaying z rosnącym currentTime reklamy)
onAdStateChange {"adState":"0","currentTime":"0", …}
nowPlaying {"state":"1","videoId":"azwz6WRaAbo","duration":"228.861", …}
```
Obie reklamy z logu (30 s i 15 s) były **niepomijalne**.

### Komendy wysłane przez Skipjacka [LOG]
Wyłącznie `setVolume` (`volume=100`, `muted=true/false`), 30× i zawsze `HTTP 200`.
`skipAd` nie poszło ani razu, bo żadna reklama nie była pomijalna.

---

## 3. Co da się zrobić (komendy `req0__sc`)

| Komenda | Parametry | Działanie | Źródło |
|---|---|---|---|
| `setVolume` | `volume` 0–100, `muted` true/false | głośność / wyciszenie | [LOG] |
| `skipAd` | – | „Pomiń reklamę”, działa tylko gdy `isSkipEnabled=true` | [PY] |
| `play` / `pause` | – | odtwarzanie / pauza | [PY] |
| `seekTo` | `newTime` (s) | przewijanie | [PY] |
| `next` / `previous` | – | następny / poprzedni z kolejki | [PY] |
| `setPlaylist` | `videoId` (+ `videoIds` = cała kolejka po przecinku, `currentTime`, `currentIndex`; `listId` dla playlisty) | włącz film i ustaw kolejkę; `currentTime` działa tylko dla pierwszego filmu | [NET] |
| `setVideo` | `videoId`, `currentTime` | włącz film (starszy wariant; `youtube-remote` najpierw wysyła `addVideo`, potem `setVideo`) | [NET] |
| `addVideo` | `videoId` | dodaj na koniec kolejki; **jeden film na request**, z losową przerwą 2–5 s między requestami, inaczej kolejka się psuje (gubi/miesza filmy) | [NET] |
| `insertVideo` | `videoId` | wstaw **za aktualnie granym** (nie na koniec!): `[zoo, bunny]` + insert rick → `[zoo, rick, bunny]` | [NET] |
| `removeVideo` | `videoId` | usuń z kolejki | [NET] |
| `clearPlaylist` | – | wyczyść kolejkę | [NET] |
| `setPlaybackSpeed` | `playbackSpeed` (np. 1.5) | prędkość | [PY] |
| `setAutoplayMode` | `ENABLED`/`DISABLED` | autoplay; **u Ciebie UNSUPPORTED** | [PY]/[LOG] |
| `setSubtitlesTrack` | `videoId`, `languageCode` | napisy | [PY] |
| `dpadCommand` | `key` = `UP`/`DOWN`/`LEFT`/`RIGHT`/`ENTER`/`BACK` | nawigacja po UI apki jak pilotem | [PY] |
| `getNowPlaying` | – | wymusza zdarzenie `nowPlaying` | [PY] |
| `getQueue` [?] | – | nazwa znana z klienta lounge w Pythonie, ale żadne niezależne źródło jej nie potwierdza; kolejkę da się odczytać inaczej (3.2) | [?] |

### 3.1a Odczyt kolejki bez komendy (casttube) [NET]
Bind zwraca w pierwszej ramce zdarzenie `playlistModified` z `videoIds` (po przecinku)
i `listId` – to migawka kolejki z momentu otwarcia sesji. Komendy odpowiadają samym
ack (`8\n[0,-1,0]`), więc żeby zobaczyć efekt komendy, trzeba zbindować się od nowa.
Alternatywa: `GET https://www.youtube.com/watch_queue_ajax?action_get_watch_queue_items=1&list=<listId>`
z nagłówkiem `X-YouTube-LoungeId-Token` zwraca HTML kolejki (parsowalne `<li class="…watch-queue-item">`).

### 3.1 Co można „zmanipulować” w praktyce
- **Pełny pilot do odtwarzania.** Play/pauza, seek, next/previous, prędkość, napisy,
  głośność, kolejka (ustaw/dodaj/wstaw/usuń/wyczyść/odczytaj) – wszystko z tabeli wyżej.
  To wystarczy, żeby zbudować własnego pilota, harmonogram („rano włącz playlistę”),
  albo integrację z domem (wycisz, gdy dzwoni telefon) [NET].
- **Reklamy niepomijalne.** `skipAd` nie zadziała, bo TV to blokuje. Realne opcje:
  - **wyciszenie** (to robi Skipjack) [LOG];
  - **[?] „przeładowanie” filmu**: na `state=1081` wysłać `setPlaylist videoId=<contentVideoId> currentTime=<pozycja>`.
    Może pominąć reklamę albo zaserwować nową, więc trzeba to przetestować;
  - **[?] `seekTo`** w trakcie reklamy: zwykle ignorowane, nie testowane;
  - **[?] `dpadCommand ENTER`**: gdy na ekranie jest przycisk „Pomiń”, to „naciśnięcie” go jak pilotem.
  - **[?] `next` w trakcie reklamy**: skoro reklama jest „filmem” w kolejce, `next` mógłby ją przeskoczyć –
    do przetestowania na TV (ryzyko: przeskoczy też film, jeśli kolejka ma 1 element).
- **Pozycja i czas.** `nowPlaying` co ~1 s daje dokładny `currentTime`. Na tym działało
  przewijanie segmentów SponsorBlock: `seekTo` na koniec segmentu.
- **Monitoring.** Co i kiedy jest oglądane (`videoId`, `listId`), ile było reklam i jakie
  (`adVideoId` to zwykły film: `https://youtu.be/-k3gN1TdXa0`), jaka jakość, jaki dźwięk.
- **Automatyzacja.** Kolejka, prędkość, napisy, głośność, nawigacja D-padem (np. auto „Tak, nadal oglądam” [?]).

### 3.2 Czego się NIE da
- Usunąć reklamy ze strumienia (reklamy po stronie serwera).
- Zalogować się ani przełączyć konta przez API (ekran wyboru konta to UI apki na TV).
- Działać, gdy apka YouTube na TV jest zamknięta (`status: unknown`). Tryb aktywny
  DIAL potrafi ją uruchomić tylko w sieci lokalnej.
- Profile dziecięce (`TVHTML5_FOR_KIDS`) zrywają się po dołączeniu pilota, więc Skipjack je pomija.

### 3.3 Ograniczenia protokołu
- `ofs` rośnie o 1 dla każdej komendy w sesji. Co się dzieje przy złym `ofs`, nie sprawdzałem [?].
- casttube twierdzi, że sesja „rozjeżdża się” po ok. 30 s i rebinduje przed każdą
  komendą kolejkową; `desvaters/ytlounge` tego nie potwierdził (sesja po 35 s bezczynności
  dalej działała) i otwiera jedną sesję na komendę [NET]. Skipjack trzyma jedną
  długą sesję z subskrypcją – gdyby komendy zaczęły ginąć, rebind przed komendą
  jest pierwszym podejrzanym [?].
- Komendy zwracają `200` od razu; o skutku informuje dopiero zdarzenie.
- Brak zdarzeń przez >60 s oznacza martwą sesję i trzeba subskrybować od nowa (watchdog).

### 3.4 „Connection denied”
Obejście: dołączyć raz jako `LOUNGE_SCREEN`, co resetuje lounge, a potem
wymusić zamknięcie apki YouTube na TV.

---

## 4. Bezpieczeństwo i prywatność (wnioski z logów)

- **`screenId` jest jak hasło do pilota.** Kto go zna, pobierze token (1.2) i może sterować
  TV z dowolnego miejsca w internecie, bez dostępu do sieci lokalnej. Nie publikuj
  `config.json`. Unieważnienie: na TV *Ustawienia → Połączone urządzenia → odłącz
  wszystkie*, a potem sparuj ponownie.
- **Log zawiera sekrety sesji.** Linia `bind response …` zapisuje pełne `SID` i `gsessionid`.
  Są krótko żyjące, ale nie wklejaj logu publicznie bez wycięcia tych linii (mogę je redagować w kodzie).
- **Log zawiera historię oglądania**: każdy `videoId`, kolejkę rekomendacji i dane TV (model, chipset).
- Na TV pilot jest widoczny pod nazwą `join_name` („Skipjack”), więc jego obecności nie da się ukryć.

---

## 5. Obserwacje specyficzne dla Twojego TV
- TCL BeyondTV2 (2021), Android 9, `clientName=TVHTML5`, apka `lb-v4`.
- Autoplay: `UNSUPPORTED`.
- Koniec reklamy przychodzi jako `adState:"0"` z `currentTime:"0"` (stąd poprawka w Skipjacku).
- Reklamę widać trzema kanałami naraz: `onAdStateChange`, `adPlaying` i `nowPlaying` ze `state=1081`.
- Token był pobierany 9× w ciągu 11 minut, czyli przy każdym Stop/Start. Zbieżność z
  wyrzucaniem do wyboru konta jest możliwa, ale nie potwierdzona.
