# Jak SmartTube „omija” reklamy — ustalenia ze źródeł

Sprawdzone na kodzie: `yuliskov/SmartTube` (master, 2026-11) + podmoduły
`MediaServiceCore` (`youtubeapi`) i `SharedModules`.

## Wniosek główny: SmartTube nie ma blokera reklam. W ogóle.

W całym repo **nie ma ani jednej klasy, ustawienia czy stringu** związanego
z reklamami. Sprawdzone:

- brak plików/identyfikatorów `adblock`, `blockAds`, `AdPlacements`, `adSlots`,
  `playerAds`, `AdBreakHeartbeatParams` (w kodzie aplikacji);
- brak zależności `exoplayer-extension-ima` (moduł IMA/VAST) — `common/build.gradle`
  wciąga tylko `exoplayer-library`, `-extension-okhttp`, `-extension-cronet`;
  jedyne trafienia `AdPlaybackState` to nieużywane testy biblioteki ExoPlayera;
- brak ustawienia „blokuj reklamy” w `strings.xml` i w `common/prefs/*`
  (jest tylko `SponsorBlockData`).

Jedyny mechanizm „usuwania” treści to **SponsorBlock** (`SponsorBlockService`,
`SponsorBlockController`) — to pomijanie segmentów sponsorsowanych w *filmie*,
nie reklam YouTube.

## Mechanizm, który daje efekt „zero reklam”

Reklamy w oficjalnej apce są **wstrzykiwane przez odtwarzacz**, nie tkwią
w strumieniu. `/youtubei/v1/player` zwraca obok `streamingData` pola
`adPlacements`, `playerAds`, `adSlots`, `adBreakHeartbeatParams`; oficjalny
player czyta je i dokłada przerwy reklamowe (mechanizm jak IMA/VAST).

SmartTube robi trzy rzeczy, które to wycinają:

1. **Nie mapuje tych pól w modelu odpowiedzi.** `videoinfo/models/VideoInfo.java`
   ma `@JsonPath` wyłącznie dla: `streamingData.formats[*]`,
   `streamingData.adaptiveFormats[*]`, `streamingData.hlsManifestUrl`,
   `streamingData.dashManifestUrl`, `captions.playerCaptionsTracklistRenderer.*`,
   `videoDetails`, `playbackTracking.*`, `playabilityStatus.*`.
   Pól reklamowych **nie ma tam wcale** — więc nie mają jak trafić do odtwarzacza.
2. **Nie ma czym ich odtworzyć.** Sam ExoPlayer bez modułu IMA nie potrafi
   odegrać przerwy reklamowej, nawet gdyby dostał metadane.
3. **Podaje się za klienta, który dostaje „chudsze” odpowiedzi.** Spooffowane
   tożsamości w `AppClient.kt` / `Constants.kt`: `TVHTML5_SIMPLY`
   (`clientName=TVHTML5_SIMPLY`, `version=1.0`, własny UA + `Referer:
   https://www.youtube.com/tv`), `TVHTML5`, `TV_SAMSUNG` (Tizen), `ANDROID_VR`
   (Oculus Quest 3, Android 12L) — z własnym UA, `X-Goog-Visitor-Id`,
   `X-Youtube-Client-Name/Version`. Do tego nowsza ścieżka streamingu **SABR/UMP**
   (własny parser w `exoplayer-amzn-2.10.6/library/sabr`).

Efekt: odtwarzane są bezpośrednio URL-e z `streamingData` (`googlevideo.com`),
a przerwy reklamowe nigdy nie powstają, bo nikt o nie nie poprosił.

## Dlaczego tego nie da się przenieść do Skipjacka

Skipjack steruje **oficjalną apką** przez Lounge. Odtwarzaczem jest apka Google,
która ma reklamy zaszyte w sobie — nie mamy wpływu na to, jakie pola czyta.
Żadna komenda Lounge nie mówi „nie odtwarzaj przerwy reklamowej”
(`LOUNGE_API.md` 3.2). Zostają obejścia z tego repo: mute, `skipAd`,
przyspieszenie odtwarzania (`PLAN-ad-time.md`), reload (`PLAN-ad-reload.md`).

## Wniosek produktowy

„Zero reklam” = inny odtwarzacz (SmartTube albo Analogiczny fork), czyli
rezygnacja z oficjalnej apki i z Lounge — a wtedy Skipjack nie ma czego robić.
To jest realny wybór: **Skipjack (oficjalna apka, reklamy wyciszone) albo
SmartTube (zero reklam, bez Skipjacka)**. Nie da się mieć obu w jednej apce.
