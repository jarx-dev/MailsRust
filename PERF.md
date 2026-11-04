# Pomiary wydajności MailRust

Build: `cargo xwin build --release --target x86_64-pc-windows-msvc` na Linuksie (NUC), rustc 1.99.1.
Exe bez ikony (brak `windres` na NUC), więc rozmiary są ~kilka KB mniejsze niż z buildu na Windows;
porównanie przed/po jest na tym samym narzędziu.

- **Przed**: commit `153e053` (po etapie 1, przed optymalizacją).
- **Po**: commit z etapu 3 (ten plik).

## Wyniki

| Pomiar | Przed | Po | Uwagi |
|---|---|---|---|
| Rozmiar exe (B) | 33 425 408 | 30 201 344 (−9,6%) | sam kod etapu 3 bez zmiany profilu: 33 422 848; z `lto="thin"` + `codegen-units=1`: 30 155 776; ostatecznie z podbitymi zależnościami (tray-icon 0.26 itd.) |
| Ostrzeżenia rustc (Windows) | 18 | 5 | martwy kod usunięty |
| Ostrzeżenia clippy (Windows) | 56 | 43 | |
| Odczyt + parsowanie HTML w oknie czytania (tryb bez przeglądarki) | w każdej klatce | raz na wiadomość | `card_blocks` |
| Zapis wektorów wyszukiwania (~8 MB / 30 tys. wiadomości) | w klatce autozapisu (co 30 s podczas analizy) | w wątku w tle | `Finder::save` |
| `stat` załączników w oknie pisania | w każdej klatce | tylko przy „Wyślij” | `compose.rs` |

## Pomiary na Windows (laptop, 78 507 wiadomości, baza 514–539 MB, NVMe)

Zrobione 2026-11-04 przez `scripts/win-test.sh` (kopia skrzynki, offline, uruchomienia na pulpicie
laptopa). „Przed” = `153e053`, „po” = `9fa09ba`. Czas od otwarcia okna.

| Pomiar | Przed | Po | Uwagi |
|---|---|---|---|
| Otwarcie pliku bazy | **4 247–4 691 ms** przy każdym starcie (prawdziwe logi), 6 717 ms w teście | **1–3 ms** (12 startów, też po zapisie) | przyczyna: baza nie była zamykana (czytniki trzymały `Arc<Database>`), redb naprawiał cały plik przy starcie; na zimnym dysku 15 s |
| Skrzynka gotowa | 4 397–4 854 ms (prawdziwe logi) | **50–63 ms** | przed pierwszą klatką: ekranu ładowania praktycznie nie widać |
| Pierwsza klatka z całą skrzynką | — | 694–812 ms | rysowanie 78 tys. wiadomości i wątków; następny kandydat |
| CPU w spoczynku (30–60 s po starcie) | 0,2–13,6% rdzenia | 0,0–0,8% rdzenia | w pierwszych ~15 s buduje się indeks wyszukiwania |
| RAM w spoczynku | 381–397 MB | 385–392 MB | bez zmiany |
| `POCZTA_BENCH` 40 s: CPU średnio / RAM max | 147% rdzenia / 648 MB | 102% rdzenia / 587 MB | po 1 przebiegu, rozrzut duży |
| Zapis bazy w wątku UI | 45–106 ms co 1–3 s przy pracy (prawdziwe logi) | bez zmian | do zrobienia: zapis w tle |

Rozrzut: kilka startów zaraz po skopiowaniu lub zmianie pliku bazy czytało wiadomości 3–8 s przy
niskim CPU (czekanie na dysk), choć sama baza czyta je w 96–141 ms (`redb-open-bench`). Pasuje to do
skanowania pliku przez ESET (`ekrn`); wyjątek w ESET dla `%LOCALAPPDATA%\Poczta\mail.redb` to decyzja
właściciela komputera.

## Profil release

- `lto="thin"` + `codegen-units=1`: −9,8% rozmiaru exe; zostawione (mniejszy plik = szybsze ładowanie,
  wpływ na czas startu do potwierdzenia na Windows). Kosztem jest dłuższy build release.
- `opt-level=3`: nie przyjęte — bez pomiaru czasu na Windows nie da się wykazać zysku.
- `panic="abort"`: odrzucone — aplikacja łapie panic (`net.rs` `guarded`, zadania w tle), abort zamieniłby
  błąd jednego zadania w zamknięcie programu.

## Spoczynek (z kodu, nie z pomiaru)

W spoczynku najczęstsze wybudzenie to `auto_sync` co 15 s; kontakty co 60 s, kalendarz co ≥5 s
tylko pod koniec 15-minutowego okna. Animacje (toast, flash, pasek pracy w tle co 1 s, spinnery)
działają tylko, gdy są widoczne. Ciągłego `request_repaint()` w spoczynku nie ma (poza trybami testowymi
`POCZTA_BENCH` i `--screenshot`).

## Co nadal dotyka dysku w wątku UI

- Zapis bazy redb (`Db::save`) przy autozapisie i po `save_now`: pisze tylko zmiany, ale na laptopie
  zajmuje 45–106 ms (78 tys. wiadomości: przegląd odcisków + `commit()` z fsync), przy pracy co 1–3 s.
  Przeniesienie do wątku wymaga zmiany modelu własności `Store` — nie zrobione.
- Jednorazowe małe odczyty przy akcji: treść z redb przy otwarciu wiadomości (`ensure_body`), plik HTML
  wybranej wiadomości (`html_blocks`), logo domeny z cache (raz na domenę), obrazki `cid:` (raz).
- `diag::log` dopisuje linię do `sync.log` synchronicznie (zdarzenia, nie klatki).
- `stat` rozmiarów załączników w panelu czytania przy każdym rysowaniu tego panelu.

## Jak mierzyć na Windows

`scripts/win-test.sh` (dostęp SSH opisany w Hindsight: „Dostęp SSH z NUC do laptopa Windows”):

1. `scripts/win-test.sh build` — buduje exe i wysyła go na laptop.
2. `scripts/win-test.sh fresh` — kopia prawdziwej skrzynki do testów (Poczta musi być zamknięta).
3. `scripts/win-test.sh run 60000` — spoczynek; `run 40000 POCZTA_BENCH=1 POCZTA_PROFILE=1` — przeglądanie;
   `run 10000 POCZTA_START=menu` — scenariusz z zapisem do bazy. Wyniki w `~/mailrust-tests/<czas>/`
   (`sync.log` z `startup:` i `database:`, `cpu-mem.csv` co sekundę).
