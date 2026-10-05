<!-- // TODO(ai-review): generated, not yet audited -->

# XCB Input Feed: Design-Entscheidungen und Roadmap

Stand: Oktober 2026. Ziel: dem Spiel gescriptete Eingaben (Tastatur/Maus)
frame-genau zuführen — Input-Injektion in die X-Connection des Spiels, als
Baustein der Frame-basierten Automation. **Noch nichts gebaut** — dieser
Text ist die decisions-Basis.

## Entscheidung: eigene Queue (Voll-Determinismus)

**Entschieden** (libTAS-Modell, ausführlich verglichen mit
`src/library/xcb/XcbEventQueue*.cpp`): Die Lib besitzt den Event-Stream —
**eigene Queue** als Default. Begründung: Mit dem Vollzustands-Modell
(Input = `movie[frame]`, siehe unten) ist Input eine reine Funktion der
Framenummer — das gilt nur, wenn *kein* anderer Event-Einfluss das Spiel
erreicht. Native Events (WM, XI2, fremde Clients) sind per Konstruktion
raus, nicht per Filter-Triage nachher. `Config.uncontrolled_input=true`
fällt auf die echte Queue + natives Passthrough zurück (Debug/„nur mal
schauen"-Modus).

Der Preis ist Lese-Pfad-Ersatz — **minimal angefangen**, siehe Roadmap:
Stufe 1 ist die kleinste eigene Queue, die funktioniert (Core-Events,
Input-Suppression, Rest durchlassen), nicht ein Vorab-Bau der libTAS-
Vollausbaustufe.

## Mechanik 1 — Drain: libxcb bleibt Socket-Besitzer

Der X-Server pusht Events aktiv auf den Client-Socket; hört niemand zu
lesen auf, füllt sein per-Client-Output-Buffer — und da Replies und Events
denselben Socket teilen, deadlockt die nächste synchrone Roundtrip (Unity
fragt massenhaft Properties). Lösung: die Hook-Fassade ruft **unter der
Haube weiter das echte `xcb_poll_for_event`** — libxcb bleibt Socket-Besitzer,
die Reply-Maschinerie (`xcb_wait_for_reply`, Sequenz-Matching, Special-
Events) läuft unberührt; wir kriegen nur Events zum Filtern: eigene Queue /
durchlassen / sofort verwerfen (verwerfen beim Drain, nicht liegen lassen).
Kein Eigen-Parsing des Protokolls, nichts jenseits der Event-API gehookt.

## Mechanik 2 — Wake: Kick-Mechanik, wiederverwendet

Blockende Waiter schlafen auf der iolock-cond; wer injiziert signalisiert
und macht einen GetInputFocus-Roundtrip — der Reply auf dem Socket weckt
den echten Wait, unsere Fassade kommt zurück in die Prüfschleife und findet
die eigene Queue nicht leer. Strukturell `valo_shim.c:310`
`valo_xcb_kick_special_event` (gebaut für den WSI-Restore-Kick), nur
reguläre Queue statt Special-Queue.

Event-Bau: `response_type` = 2 (KeyPress) / 3 (KeyRelease) usw., ans Ende
der eigenen Queue, `full_sequence` (das u32 *hinter* dem Wire-Event, Teil
von `xcb_generic_event_t`, muss mitalloziert werden) auf 0 (siehe TODO
unten), `event.time` **aus der virtuellen
Uhr** (det-time existiert schon) — sonst ist jede Reproduktion von Frame N
nicht bitidentisch.

## Mechanik 3 — Xlib-Brücke: gratis mitgeliefert, zwei Fußangeln

libX11 ist auf libxcb aufgebaut (`Display` enthält die xcb-Connection,
`XNextEvent` füllt via `xcb_wait_for_event` nach) — ein Xlib-Spiel läuft
durch unsere xcb-Hooks ohne eigenes Zutun (im HK-GLX-Log verifiziert: die
Xlib-Session nutzte die gehookten xcb-Symbole). Fußangeln:

- libX11s eigener Puffer liegt obendrauf — injizierte Events landen hinter
  bereits gepufferten.
- `XCheckIfEvent`/`XPeekEvent` iterieren libX11s Puffer, nicht unseren
  Poll — erste Verdächtige, wenn ein Xlib-Spiel Injektionen übersieht.

## Input = Vollzustand pro Frame (libTAS `AllInputs`)

Der Client trägt den Input-Zustand für den nächsten Frame in seine
bestehende Boundary-Antwort (`AdvanceFrames(n)`) — kein eigener Kanal, kein
extra Roundtrip (libTAS nutzt Shared Memory gegen Syscalls; die Socket-
Piggyback-Variante ist bei 60fps bereits messtechnisch frei). **Events sind
Derivat, nicht Quelle**: Die Lib hält den zuletzt geservten Zustand, diffed
gegen den neuen (Zustandsübergang → Tasten-Presse/-Release, Maus-Delta →
Motion) und injiziert genau diese synthetisierten Events.

Der Grund ist der Savestate: Input wird so eine reine Funktion der Frame-
nummer (`movie[frame]`). Nach einem Restore auf Frame N gilt wieder
derselbe Zustand, der Diff reproduziert dieselben Events — bei Event-
Kommandos (`SendKey`) spult der Restore den Event-Stream zurück, Keys
bleiben ohne KeyRelease stecken, Modifier-State driftet. Bonus: der
„zuletzt geservte Zustand" ist ein plain static in der Lib → snapshottet
mit, rewindet mit dem Restore. Der Savestate ist auch der Grund für die
eigene Queue: native Events, die nur in einem Lauf ankommen, machen
`movie[frame]` kaputt.

## Was das Event braucht (und woher valo es nimmt)

- **Window-XID**: das Zielfenster. Tracken via `xcb_create_window`-Hook
  oder vom aktiven Fenster der getrackten Connection; HK/Unity hat genau
  ein Game-Fenster.
- **Keycode**: Keysym→Keycode ist Server-Layout-abhängig — der Client
  macht es vor (`client/src/input.rs`: `GetKeyboardMapping` beim Attach).
  Skripte speichern **Keysyms**, erst die Lib mappt bei Injektion
  (Layout-Unabhängigkeit).
- **Modifier-State**: `event.state` aus der gehaltenen Modifier-Menge
  synthetisieren (Shift/Ctrl…), pro gedrückter Taste pflegen.
- **Ziel-Connection**: `GAME_XCB_CONNS` (hooks/xcb/conn.rs) trackt die
  Verbindungen des Spiels (max 4).

## XI2 als bekannter Bodensee

HK/Unity läuft auf XI2 (der ge_generic-Event-Flood in den Logs): KeyEvents
kommen als `XIDeviceEvent` (XGE, response_type 35 + deviceid), nicht als
Core-KeyPress. Spiele mit XI2-Abo lesen Core-Events teils gar nicht. Für
HK heißt das: der Input-Feed muss XIDeviceEvents synthetisieren (Struct in
`xcb/xinput.h`, deutlich größer, deviceid/evtype sauber setzen) — Stufe 0
klärt, ob das von Anfang an nötig ist.

## Roadmap (beschlossen: eigene Queue als Ziel, minimal anfangen)

1. **Stufe 0 — XTest-Sondierung (null Lib-Arbeit)**: Client verbindet
   selbst auf `DISPLAY`, `XTestFakeKeyEvent`. Klärt die Vorfrage: liest HK
   überhaupt Core-KeyPress (XI2-Verdacht)? Nicht deterministisch — nur
   Sondage, kein Baustein.
2. **Stufe 1 — kleinste eigene Queue**: Poll/Wait-Peeks + eigene Queue
   (erst eigene, dann echtes Poll als Drain darunter), Filter minimal =
   Input-Events (Core + XI2) unterdrücken, Rest durchlassen (Expose/
   ConfigureNotify braucht das Spiel fürs Fenster). `InputState` im
   `AdvanceFrames`-Reply, Zustands-Diff → synthetisierte Core-Events,
   `event.time` virtuell. `uncontrolled_input=true` = Fassade aus,
   natives Verhalten.
3. **Mini-TAS-Framework**: deterministisches Testspiel (per-Frame-Output →
   per-Frame-Diff = Orakel v0) + Frame-Skript in Keysyms (`10 1,D 10 10,E`).
   Die drei Tests: **Restore mitten im Script** (Savepoint N, Script bis M,
   Restore auf N, nochmal bis M → identischer Output = Invarianz
   Input+Savestate), **Skip-Invarianz** (mit/ohne `fast_forward` →
   identisch — damit ist FF *bewiesen*, nicht nur schnell), **Layout-
   Unabhängigkeit** des Skripts.
4. **Ausbaustufen bei Bedarf**: XI2-Synthese (falls Stufe 0 es zeigt),
   Suppression-Politik verfeinern (fremde ClientMessage, XI2-Raw),
   `XCheckIfEvent`-Familie, Wait-Feintuning (nativer Fallback statt Spin).
5. Danach: Det-Time fertigstellen (Config-Migration, Sleep-Virtualisierung,
   siehe `deterministic-time.md`), Orakel für echte Spiele (RNG-State,
   Frame-Traces, Snapshot-Diff), Uncapped-mit-Video (siehe
   `explicit-sync.md`).

## TODO: `full_sequence` gültig stempeln statt 0

Gefütterte Events laufen auch durch Xlibs eigenes `xcb_poll_for_event`/
`xcb_poll_for_queued_event`. libX11 (`xcb_io.c`, `poll_for_event`) ergänzt
`full_sequence` per `widen()` relativ zu `last_request_read` und bricht ab,
wenn das Ergebnis über `request` liegt ("Unknown sequence number while
processing queue", `xcb_xlib_threads_sequence_lost`). Uninitialisiertes
`full_sequence` hat genau das bei HK ausgelöst.

0 besteht die Prüfung, ist aber nicht sauber:

- Xlib übernimmt den Wert: `last_request_read` springt auf 0 zurück. Bis zur
  nächsten echten Antwort wirkt die Request-Spanne riesig, `sync_hazard`/
  `_XSeqSyncFunction` (`XlibInt.c`) erzwingen dann `GetInputFocus`-Roundtrips.
- Ab `last_request_read > 2^31` wertet `widen()` die 0 als Wrap (+2^32) →
  wieder dieselbe Assertion.

Richtig wäre `full_sequence` = aktuelles `last_request_read` des Displays,
das die Connection besitzt (Event gilt dann als direkt nach dem zuletzt
Gelesenen angekommen; `request` wäre zwar auch im Fenster, hält das Event
aber hinter offenen Replies fest). Braucht eine Zuordnung Connection →
Display (z. B. `XGetXCBConnection` über die bekannten Game-Displays) und
muss beim Pop gestempelt werden, nicht beim Push. Reine xcb-Connections
prüfen `full_sequence` nicht, dort bleibt 0.
