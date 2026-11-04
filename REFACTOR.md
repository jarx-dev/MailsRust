# Refaktor MailRust: plan

Tryb: /ponytail full. Dwa cele: (A) rozbić monolit app.rs na moduły, (B) zoptymalizować
aplikację pod szybkość, małe zużycie RAM i CPU oraz płynne UI. Zachowanie aplikacji nie może się zmienić.

## Kontekst

- Repo git jest w `app/`, gałąź `main`, z commitem bazowym. Każdy etap to osobny commit.
- Zachowanie aplikacji (czasy, reguły czytania i usuwania wiadomości) ma zostać bez zmian.
- Testów nie ma.
- `src/app.rs` ma ~9500 linii. Prawie wszystko to jeden `impl MailApp` (linie ~797–8377)
  plus `impl eframe::App` (~8566–9030) i wolne funkcje pomocnicze.

## Stan (2026-11-04)

- Etap 1 zrobiony (commity „Etap 1: …”): `src/app/` = `mod.rs` (rdzeń, 1487 linii) + `compose`,
  `reading`, `list`, `sidebar`, `sync`, `actions`, `contacts_ui`, `settings`, `window`; tryby CLI
  z `main()` w `src/cli.rs`; `net.rs` bez zmian (części splecione). Build release i clippy bez
  nowych ostrzeżeń (bazowo 18 ostrzeżeń rustc).
- Etap 3 zrobiony w zakresie możliwym bez Windows (commity „Etap 3: …”, opis w `PERF.md`): HTML okna
  czytania parsowany raz, wektory wyszukiwania zapisywane w tle, martwy kod usunięty (rustc 18 → 5
  ostrzeżeń), profil `lto="thin"` + `codegen-units=1` (exe −9,8%).
- Do zrobienia na Windows: ręczny test aplikacji (etap 1). Pomiary etapów 2 i 4 zrobione zdalnie
  przez `scripts/win-test.sh` (SSH do laptopa), wyniki w `PERF.md`; start skrócony z ~4,5 s do ~60 ms
  (baza zamykana poprawnie). Zostaje zapis redb w wątku UI (45–106 ms przy pracy).
- Aplikacja jest tylko pod Windows. Na Linuksie (NUC) sprawdzisz ją przez
  `cargo check|clippy --release --target x86_64-pc-windows-msvc`, a exe zbudujesz przez
  `cargo xwin build --release --target x86_64-pc-windows-msvc` (target `-gnu` wymaga `dlltool` z
  Windows, patrz `.cargo/config.toml`). Uruchomienie, zrzuty (`--screenshot` + `POCZTA_START=…`)
  i pomiary czasu, RAM i CPU wymagają Windows.

## Etap 1: podział app.rs (najpierw, przed optymalizacją)

Technika: zamień `app.rs` na `app/mod.rs` (albo `app.rs` + folder `app/`). Tematyczne części
przenieś do podmodułów, każdy z własnym blokiem `impl MailApp { ... }`. Podmoduły widzą
prywatne pola MailApp, więc nie trzeba zmieniać widoczności pól ani dodawać getterów, traitów czy wrapperów.

Proponowane granice, oparte na istniejących funkcjach (skoryguj według faktycznych zależności):

| Moduł | Zawartość |
|---|---|
| `compose.rs` | `Compose`, `Original`, `open_composer`, `composers_ui` (~1050 linii; podziel ją na kilka funkcji tylko tam, gdzie już ma wyraźne sekcje), `editor_ui`, `signature_*`, `check_signature` |
| `reading.rs` | `reading_pane`, `email_view`, `render_html`, `html_blocks`, `image_bytes`, `picture`, `readers_ui`, `previews_ui`, `read_after_a_while`, `ReaderWin`, `PreviewWin` |
| `list.rs` | `message_list`, `message_row`, `message_menu`, `vis_list`, `visible*`, `thread_*`, `later_*`, `counts`, `refresh_caches`, `VisList`, `Row`, `Counts` |
| `sidebar.rs` | `top_bar`, `nav_bar`, `folder_list`, `side_bar`, `folder_row*`, `tag_*`, ikony |
| `sync.rs` | `poll_net`, `poll_loading`, `send_receive`, `auto_sync`, `watch_inbox`, `start_receive`, `sync_slot`, `queue_op`, `flush_ops`, `poll_email`, `start_loading` |
| `actions.rs` | `Action`, `apply_actions`, `neighbour`, `go_to_neighbour` |
| `contacts_ui.rs` | `contacts_side`, `contacts_main`, `person_editor`, `book_*`, `mail_people`, `mail_contacts` |
| `settings.rs` | `account_ui`, `extra_account_ui`, `side_settings_ui`, `ai_settings_ui`, `gcal_settings_ui`, `gcal_*` |
| `window.rs` | `WinButton`, `window_button`, `window_edges`, tray (`make_tray`, `TrayMsg`), klawiatura i fokus (`keyboard`, `window_keyboard`, `focus_state`…) |
| `mod.rs` | `MailApp`, `Prefs`, `new()`, `impl eframe::App`, `save`, wspólne małe helpery |

Zasady:
- TYLKO przenoszenie kodu, bez zmian logiki i nazw.
- Jeden moduł = jeden commit. Po każdym commicie `cargo build --release` bez nowych ostrzeżeń.
- Na koniec etapu `cargo clippy` i ręczny test aplikacji: start, lista, otwarcie maila,
  odpowiedź, wysyłka, kalendarz, kontakty, ustawienia.
- Sprawdź też `net.rs` (1339) i `main.rs` (1052). Dziel je tylko tam, gdzie są wyraźnie osobne części, bez dzielenia na siłę.

## Etap 2: pomiar bazowy na buildzie release

Zmierz czas startu do pierwszej klatki, RAM i CPU w spoczynku, RAM i CPU przy przewijaniu listy
i otwieraniu maila HTML, rozmiar exe. Zapisz wyniki w `PERF.md`.

## Etap 3: optymalizacja

- Uruchom /ponytail-audit pod kątem wydajności: praca na wątku UI (IMAP, redb, parsowanie
  HTML i obrazów w update/ui), zbędne `request_repaint` i ciągłe odświeżanie w spoczynku,
  klonowania i alokacje w każdej klatce, rysowanie list bez wirtualizacji, dekodowanie obrazów bez cache,
  blokujące I/O.
- Napraw problemy od największego zysku: najmniejsza zmiana, bez nowych zależności
  i bez nowych abstrakcji. Usuń martwy kod i duplikaty.
- Profil release: sprawdź `opt-level=3`, `lto="thin"`, `codegen-units=1` i `panic="abort"`.
  Zostaw tylko to, co mierzalnie pomaga.
- Dla nietrywialnej logiki, którą zmieniasz, dodaj mały test.

## Etap 4: pomiar końcowy

Powtórz pomiary z etapu 2 i dopisz do `PERF.md` tabelę przed/po.

## Cel osiągnięty, gdy

- żaden plik w `src` nie przekracza ~1500 linii, a `mod.rs` zawiera tylko rdzeń,
- `cargo build --release` i `cargo clippy` przechodzą, aplikacja działa jak wcześniej,
- żadna klatka UI nie czeka na sieć ani dysk,
- CPU w spoczynku jest bliskie 0%,
- `PERF.md` pokazuje poprawę.

Na koniec wypisz, czego nie zrobiłeś lub nie sprawdziłeś i jakie widzisz ryzyka.
