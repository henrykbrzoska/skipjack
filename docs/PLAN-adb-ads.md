# Plan 4: blokowanie reklam przez ADB — co realnie da się zrobić

TV: TCL BeyondTV2, **Android 9, bez roota**. To jest kluczowe ograniczenie:
bez roota nie ma `iptables`, nie ma edycji `/system/etc/hosts`, nie ma
instalacji CA do magazynu systemowego. Więc „blokowanie reklam YouTube na
poziomie sieci z ADB” **nie zadziała** — reklamy lecą z tych samych domen co
film (`googlevideo.com`, `youtube.com`), a filtrowanie po SNI/DPI nie odróżni
reklamy od contentu.

Ale ADB daje **trzy rzeczy, których Lounge API nie daje** — i jedna z nich
rozwiązuje Twoje dwa realne problemy (reklamy niepomijalne, ekran wyboru konta).

---

## 4.1 Poziom 1 — ADB jako „palec” (największa wartość, mały koszt)

`adb shell input` to prawdziwy pilot dotykowy, niezależny od Lounge:

| Cel | Komenda | Uwaga |
|---|---|---|
| „Pomiń reklamę” | `input tap <x> <y>` | działa też tam, gdzie `skipAd` odmawia; pozycję trzeba znaleźć raz na model TV |
| „Czy nadal oglądasz?” | `input tap <x> <y>` lub `input keyevent DPAD_CENTER` | koniec z pauzą przy bezczynności |
| **Ekran wyboru konta** | `input tap <x> <y>` | to jest Twój błąd „wywala do wyboru konta” — ADB może to kliknąć z powrotem |
| Nawigacja | `input keyevent DPAD_UP/DOWN/LEFT/RIGHT/ENTER/BACK` | odpowiednik `dpadCommand` z Lounge, ale działa też gdy lounge padnie |
| Tekst | `input text "..."` | szukanie w apce |

Do znalezienia współrzędnych: `adb exec-out screencap -p > shot.png` (obejrzę
i podam koordy), ewentualnie `adb shell uiautomator dump /sdcard/ui.xml`
**[verify]** — apka YouTube TV (`lb-v4`) rysuje część UI poza drzewem
dostępności, więc dump może być pusty; wtedy zostaje stała pozycja + diff
dwóch screenshotów.

**Koszt:** ~150 linii (moduł `adb.rs`: `connect`, `screencap`, `tap`, `key`).
**Ryzyko:** niskie (nic nie instalujemy, nic nie wyłączamy), ale wymaga
włączonego „ADB debugging” na TV — czyli otwartego portu 5555 w LAN.

---

## 4.2 Poziom 2 — ADB jako „oczy” (diagnostyka źródeł reklam)

To jest odpowiedź na Twoje pytanie „czy z tego poziomu zobaczyć źródła adsów”:
**zobaczyć tak, zablokować nie.**

- `adb logcat` podczas reklamy — apka TV loguje żądania reklamowe (ExoPlayer/IMA).
  Da się wyciągnąć hosty, ścieżki, parametry `adNextParams` w czytelnej formie,
  czego Lounge nie pokazuje (tam mamy tylko `adVideoId`).
- Systemowy proxy: `settings put global http_proxy <ip>:<port>` → własny proxy
  na PC, loguje **SNI** każdego połączenia (bez MITM, bez certów). Zobaczymy,
  które domeny TV odpytuje przy reklamie. **[verify]** czy apka YouTube TV
  respektuje systemowy proxy (Cronet zwykle tak).
- Efekt uboczny: te logi można wrzucić do statystyk Skipjacka
  („skąd przyszła ta reklama”) — to rozwija `docs/PLAN-ad-sources.md`.

**Koszt:** 1–2 h ręcznej roboty + skrypt parsujący logcat.
**Ryzyko:** zero (tylko odczyt), poza tym że logcat przy reklamach jest głośny.

---

## 4.3 Poziom 3 — ADB jako instalator (jedyna *prawdziwa* blokada)

Skoro nie da się filtrować reklam w locie, jedyne realne rozwiązanie to
**inna apka, która nie pobiera reklam**:

- `adb install SmartTube.apk` (SmartTube, `com.liskovsoft.smarttubetv.beta`) —
  open source, na Android TV, blokuje reklamy natywnie, wspiera SponsorBlock.
- **Ale:** to zamiennik apki YouTube, więc sesja `TVHTML5` znika i **Skipjack
  staje się zbędny** (SmartTube sam robi to, co robi Skipjack).
- Można trzymać obie apki i przełączać się, ale wtedy dwa różne piloty.

**To jest decyzja produktowa, nie techniczna:** albo Skipjack (oficjalna apka,
reklamy wyciszone, zero reklam nie da się), albo SmartTube (zero reklam,
Skipjack niepotrzebny). Warto to świadomie wybrać, zamiast dokładać kod.

---

## 4.4 Czego ADB **nie** zrobi (sprawdzone ograniczenia)

| Pomysł | Dlaczego nie |
|---|---|
| edycja `/system/etc/hosts` | brak roota, partycja tylko do odczytu |
| `iptables` / blokada IP | brak roota |
| prywatny CA do MITM HTTPS | od Androida 7 apki nie ufają certom użytkownika; YouTube TV nie opt-inuje |
| Private DNS (`settings put global private_dns_mode hostname`) | Android 9 to ma, ale reklamy YouTube idą z `googlevideo.com` — filtr DNS ich nie odetnie; zablokowanie tej domeny zabija też film |
| `pm disable-user` na komponentach Play | YouTube TV wymaga GMS, grozi cegłą apki |
| blokada po IP | CDN Google dzieli IP z contentem |

---

## 4.5 Rekomendacja i kolejność

1. **Włącz ADB na TV i podłącz** (`adb connect <ip>:5555`) — bez tego nic nie sprawdzę.
2. **Faza 0 (30 min):** `screencap` + `uiautomator dump` przy reklamie i przy
   ekranie wyboru konta. Ustalić, czy da się klikać po współrzędnych.
   → Jeśli tak: to najtańsza poprawa jakości życia, jaką mamy (koniec z ręcznym
   klikaniem po wyrzuceniu do wyboru konta i **prawdziwe** pomijanie reklam
   pomijalnych).
3. **Faza 1 (1–2 h):** logcat + proxy podczas 5 reklam → raport „skąd lecą reklamy”
   (domyka `PLAN-ad-sources.md`).
4. **Faza 2 (decyzja):** SmartTube zamiast Skipjacka, czy zostajemy przy oficjalnej apce.

**Bezpieczeństwo:** ADB na `5555` bez hasła = każdy w LAN może sterować TV
i instalować apki. Po testach wyłączyć „ADB debugging” na TV albo trzymać
`adb kill-server` i włączać tylko na czas zabawy.

**Status:** nic nie implementowane — czekam na podłączony TV, bo każde
twierdzenie z 4.1/4.2 wymaga potwierdzenia na Twoim modelu.

**Uzupełnienie:** jak dokładnie SmartTube osiąga „zero reklam” (i dlaczego to
nie jest bloker, tylko brak odtwarzacza reklam) — `NOTES-smarttube.md`.
