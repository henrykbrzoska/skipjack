# Plan 3: czy z Lounge API da się blokować źródła reklam — odpowiedź: nie

Lounge to poziom **pilota**, nie sieci:

- Nie dostajemy żadnych URL-i serwerów reklamowych — tylko `adVideoId`
  (zwykłe videoId, np. `youtu.be/-k3gN1TdXa0`), puste `clickThroughUrl`,
  nieprzezroczyste `adNextParams`/`remoteSlotsData`, `adSystem:"0"`.
- Nie ma komendy „zablokuj źródło”, nie ma wglądu w strumień — reklamy wstrzykuje
  serwer, TV je tylko odtwarza.
- `watch_queue_ajax` zwraca tylko kolejkę, nie źródła reklam.

## Co da się jeszcze sprawdzić (tanio, 1–2 h)

1. Zalogować pełne `adPlaying` dla 10–20 reklam i zobaczyć, czy
   `adNextParams`/`remoteSlotsData`/`adSystem` kiedykolwiek zawiera URL/host
   (dotąd nie).
2. Sprawdzić, czy `adVideoId` resolve'uje się przez oEmbed/Invidious na coś
   z opisem reklamodawcy (ciekawostka do statystyk, nie blokada).
3. Prawdziwa blokada to inny poziom: DNS (Pi-hole/AdGuard Home — YouTube słabo,
   bo reklamy lecą z tych samych domen), blokada na routerze, SmartTube na TV
   albo Premium. Lounge może co najwyżej wspierać: mute + statystyki + `next`/reload.

## Wniosek

Nie implementować „blokowania źródeł” w Skipjacku — API tego nie daje.
Zostaje: mute (działa), statystyki (działają), eksperyment reload (Plan 2).

**Manipulacja czasem:** zegar systemowy nic nie daje (reklamy używają zegarów
monotonicznych i pozycji odtwarzania) — zobacz `PLAN-ad-time.md`. Tam też
znacznie lepszy trop: **przyspieszenie odtwarzania reklamy** (`setPlaybackSpeed`),
które skraca reklamę realnie i jest odwracalne.


**Źródła reklam da się natomiast *zobaczyć* przez ADB** (`logcat`, proxy po SNI) —
szczegóły i ograniczenia w `PLAN-adb-ads.md` (4.2). Zablokować ich z poziomu TV
bez roota nie da się.

