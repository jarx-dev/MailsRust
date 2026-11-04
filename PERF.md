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
| Start do pierwszej klatki | nie zmierzone | nie zmierzone | wymaga Windows, patrz niżej |
| RAM / CPU w spoczynku | nie zmierzone | nie zmierzone | wymaga Windows |
| RAM / CPU przy przewijaniu i HTML | nie zmierzone | nie zmierzone | wymaga Windows |

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

- Zapis bazy redb (`Db::save`) przy autozapisie co 30 s i po `save_now`: pisze tylko zmiany, ale
  `commit()` czeka na fsync. Przeniesienie do wątku wymaga zmiany modelu własności `Store` — nie zrobione.
- Jednorazowe małe odczyty przy akcji: treść z redb przy otwarciu wiadomości (`ensure_body`), plik HTML
  wybranej wiadomości (`html_blocks`), logo domeny z cache (raz na domenę), obrazki `cid:` (raz).
- `diag::log` dopisuje linię do `sync.log` synchronicznie (zdarzenia, nie klatki).
- `stat` rozmiarów załączników w panelu czytania przy każdym rysowaniu tego panelu.

## Jak zmierzyć na Windows (etap 2/4)

Zbuduj oba commity (`git checkout 153e053` i bieżący) i dla każdego exe:

1. Start: uruchom, w `%LOCALAPPDATA%\Poczta\sync.log` odczytaj
   `startup: first screen drawn in N ms, M ms after opening` (średnia z 5 startów).
2. Spoczynek: po 60 s od startu, w PowerShell:
   `$p=Get-Process mailclient; $a=$p.CPU; Start-Sleep 60; (Get-Process mailclient).CPU-$a` (s CPU na minutę)
   oraz `(Get-Process mailclient).WorkingSet64/1MB`.
3. Przewijanie i HTML: `set POCZTA_PROFILE=1` i `set POCZTA_BENCH=1`, uruchom na ~60 s; log co 3 s ma
   `profile <część>: X ms per call, longest Y ms` oraz `memory now: N MB`. CPU jak w p. 2.
4. Wpisz liczby do tabeli wyżej.
