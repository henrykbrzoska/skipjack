# Plan 5: manipulacja czasem — jak apka YouTube na Android TV mierzy czas

Pytanie: da się „oszukać zegar”, żeby reklamy leciały szybciej / kończyły się wcześniej?

Krótka odpowiedź: **zegar systemowy to ślepa uliczka** (bez roota nie zmienisz,
a reklamy i tak go nie używają). Ale istnieje **analog, który działa na tym samym
pomyśle i jest tani do przetestowania: manipulacja czasem *odtwarzania***
(patrz 5.4). To najbardziej obiecujący pomysł na skrócenie reklam niepomijalnych,
jaki mamy.

---

## 5.1 Pięć zegarów, których używa apka (i co je rusza)

| Mechanizm | Co to jest | Czy reaguje na zmianę zegara systemowego |
|---|---|---|
| `System.currentTimeMillis()` | wall clock (UTC z `CLOCK_REALTIME`) | **tak** (ale zmiana wymaga roota) |
| `SystemClock.elapsedRealtime()` | monotoniczny od bootu, **nie śpi** w deep sleep | nie |
| `SystemClock.uptimeMillis()` | monotoniczny od bootu, **nie liczy** deep sleep | nie |
| `System.nanoTime()` | monotoniczny, dowolny punkt startu | nie |
| Pozycja odtwarzacza (ExoPlayer `getCurrentPosition()`) | zegar **medialny**: pozycja w strumieniu | nie |

Wniosek nr 1: cała logika związana z odtwarzaniem (w tym reklamą) siedzi na
zegarach **monotonicznych i medialnych**, a nie na wall clock. Przesunięcie daty
w Androidzie nie zmienia ani jednego ticka w tych trzech pierwszych.

## 5.2 Jak liczony jest czas reklamy

Reklama w YouTube TV to zwykły strumień wideo + metadane VAST/IMA:

- `isSkipEnabled` i próg pojawienia się przycisku „Pomiń” to **`skipoffset`
  z VAST**, liczony w czasie **odtworzonym** (media position), nie w zegarze
  ściennym [NET];
- licznik „Pomiń za 5… 4… 3…” to `skipoffset - playerPosition`;
- `duration` reklamy z `adPlaying` (np. `"30.021"`) to długość strumienia;
- „obejrzano” potwierdzają **beacony lecące do serwera reklamowego** (start,
  kwartyle 25/50/75%, complete). Serwer nie ufa zegarowi klienta — dostaje
  własne znaczniki czasu.

Wniosek nr 2: żeby reklama skończyła się wcześniej, trzeba ruszyć **czas
odtwarzania**, nie zegar. `seekTo` w trakcie reklamy TV ignoruje
(`LOUNGE_API.md` 3.1), więc zostaje zmiana **tempa** odtwarzania — 5.4.

## 5.3 Czy w ogóle da się zmienić zegar na tym TV (bez roota)

| Metoda | Realne? | Uwagi |
|---|---|---|
| `adb shell date …` | **nie** | wymaga `CAP_SYS_TIME`/roota na Androidzie 9 |
| `settings put global auto_time 0` | tylko wyłącza synchronizację | nie ustawia czasu |
| NTP spoofing w LAN | **tak, obejściem** | TV z `auto_time=1` pyta `time.android.com`/`time.google.com`; przekierowanie DNS na własny NTP przesuwa zegar bez roota |
| Frida/Xposed hook `currentTimeMillis` | nie bez roota | wymaga `frida-server` (root) lub przepakowania APK (łamie podpis i DRM) |
| Zmiana strefy czasowej | trywialna, bezużyteczna | nic nie zmienia w logice reklam |

NTP spoofing jest wykonalny, ale **szkodliwy, nie pomocny**:
- przesunięcie o więcej niż kilka minut psuje walidację certyfikatów TLS →
  YouTube przestaje się łączyć (a my tracimy lounge i cały Skipjack);
- nawet małe przesunięcie nic nie daje, bo reklamy nie patrzą na wall clock
  (5.1–5.2);
- do tego rozjeżdża `expiration` tokenów lounge i logi.

Gdzie wall clock **ma** znaczenie (czyli gdzie przesunięcie by coś zmieniło):
okna premier/live, „obejrzyj później”, wygasanie tokenu lounge (liczone
serwerowo, więc nie), harmonogram zadań samego Skipjacka. Żadne z tych nie
skraca reklamy.

## 5.4 Właściwy trop: czas **odtwarzania**, nie zegar

Skoro próg „Pomiń” i koniec reklamy liczą się w czasie odtworzonym, to
**przyspieszenie odtwarzania reklamy** daje dokładnie ten efekt, o który pytasz:

- `setPlaybackSpeed playbackSpeed=2` (albo więcej, jeśli TV przyjmie) na
  `state=1081` → 30-sekundowa reklama mija w 15 s, a przycisk „Pomiń” (jeśli
  jest) pojawia się dwa razy szybciej w czasie rzeczywistym;
- na powrocie do `state=1` natychmiast `setPlaybackSpeed 1` — inaczej film
  zostanie na 2× (i to jest realne ryzyko tej metody);
- jeśli TV obsługuje tylko 0.25–2×, sufit to połowa czasu reklamy; warto
  sprawdzić 4×/8×/16×, bo `setPlaybackSpeed` przyjmuje dowolną liczbę, a
  serwer może ją obciąć do najbliższej dostępnej [verify].

**Kryterium sukcesu:** `adPlaying.duration=30` → zdarzenie `onAdStateChange
adState=0` po ~15 s zamiast ~30 s, przy `speed=2`. Przy `speed=4` — po ~7.5 s.

**Ryzyko:** zerowe dla treści (odwracalne jedną komendą), poza scenariuszem
„nie zdążyliśmy wrócić do 1×”. Dlatego implementować razem z watchdogiem:
jeśli `state=1` i `speed != 1`, wyślij `1` (idempotentnie).

To jest też **lepsze niż reload** (`PLAN-ad-reload.md`): reload może zaserwować
nową reklamę, speed tylko skraca tę, która leci, i nie rusza kolejki.

## 5.5 Co jeszcze warto zmierzyć przy okazji

1. Czy `nowPlaying.currentTime` rośnie z prędkością ×2 (potwierdzenie, że to
   zegar medialny — tak powinno być).
2. Czy `onPlaybackSpeedChanged` wraca z `1` samoczynnie po reklamie (jeśli tak,
   watchdog niepotrzebny).
3. Czy przy `speed=2` beacony reklamowe dalej lecą normalnie — z naszego punktu
   widzenia bez znaczenia, ale tłumaczy, czemu to nie „psuje” konta.
4. Czy `setPlaybackSpeed` w trakcie reklamy jest w ogóle przyjmowane (200 ≠
   wykonane — patrz `LOUNGE_API.md` 3.3).

## 5.6 Kolejność roboty

1. Transport z `PLAN-remote-control.md` (tam `set_speed` już jest w zakresie).
2. Flaga `experimental_speedup_ads: false` + log `duration` → czas do
   `adState=0` (samo logowanie, zero ryzyka, dane do decyzji).
3. Jeśli dane pokażą skrócenie — domyślnie ON dla reklam niepomijalnych,
   z watchdogiem przywracającym `1×`.
