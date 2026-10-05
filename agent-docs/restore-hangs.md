# Restore-Hangs: Fallklassen, Subsysteme, Diagnose

Wissen aus der WSI/Multi-Cycle-Session (Okt. 2026). Ziel: bei künftigen
Hangs schneller klassifizieren, an welchem Subsystem es liegt. Die
konkreten Fixes stehen in den Commits (nk/zo/xp/tk) und im HANDOVER.md.

## Das Grundprinzip, das alle Fälle erklärt

Ein Restore spült **Userspace-Zustand** auf den Checkpoint-Stand zurück —
aber drei Dinge werden **nicht** zurückgespült:

1. **Kernel-seitige Waiter**: ein Thread im `futex_wait`/`poll` schläft im
   Kernel. Der Memory-Restore ändert seine Erwartung nicht und weckt ihn
   nicht — nur ein echter `FUTEX_WAKE`/Daten-/Signalanstoß tut das.
2. **Externe Prozesse**: X-Server/XWayland/Satellite, Compositor — deren
   State (Sequence-Counter, registrierte Event-Contexts, gehaltene
   wl_buffer, Pending-Presents) läuft weiter bzw. bleibt stehen.
3. **Post-Checkpoint erzeugte Objekte**: Threads, Resource-IDs, Registry-
   Einträge, die zwischen Checkpoint und Restore entstanden, kennt der
   rewoundene Zustand nicht.

Daraus folgen alle beobachteten Hang-Klassen.

## Fallklassen

### 1. Sequence-Divergenz (xcb) → „NULL ohne Fehler"

**Symptom:** erste xcb-Roundtrip-Abfrage nach dem Restore liefert NULL
ohne `has_error` (mesa WSI → SURFACE_LOST → `swapchain_ready=false` →
busy-poll ohne Neuanlauf).
**Ursache:** Client-Counter (out.request/in.request_read) rewound, Server
weitergelaufen. Erste Reply zieht `in.request_read` über alle post-
Restore-Cookies → `poll_for_reply` behandelt sie als „schon abgeholt".
**Fix:** Connection-Counter + Queues aus Restore-Zeit, Heap-State aus
Checkpoint (siehe nk).
**Lerne:** xcb-native Apps (vkcube, viele Games) tracken nicht über
XOpenDisplay — Hook muss BOTH PLT und dlsym abdecken.

### 2. XID-Allocator-Rewind → doppelte Resource-Ids

**Symptom:** Recreate bricht; xtrace zeigt doppelte `shmseg=`/`pixmap=`
Ids; Server antwortet mit X-Errors.
**Ursache:** Allocator rewound, aber Server-seitige Resources aus der
verworfenen Timeline existieren weiter.
**Fix:** XID-Allocator = I/O-Progress-Seite (aus Restore-Zeit), nicht
Checkpoint-Seite. Allgemeiner: **jeder Client-seitige Zähler, dessen
Werte der Server gesehen hat, gehört zur I/O-Progress-Seite.**

### 3. Geparkter Special-Event-Consumer → Destroy-Join-Hang

**Symptom:** main hängt in `thrd_join` des Event-Managers; der Manager
parkt in `xcb_wait_for_special_event` (poll, Reader-Rolle) und re-checkt
`chain->status` erst nach der Rückkehr. Events fehlen, weil die Timeline
sie konsumiert hat. **Resize/Visibility hilft manchmal** (XWayland
completing Presents nur bei Compositor-Fortschritt), ist aber nicht
deterministisch.
**Fix:** Client-seitige Injektion eines Events in die Queue + blocking
Roundtrip (Reply weckt den Poller). Timing kritisch: Kick muss NACH dem
Status-Write des Destroy landen — Einmalkicks racen, deshalb Budget +
Verbrauch am Join.

### 4. Cond-Buchhaltung rewound, Kernel-Waiter schläft weiter ⚠️

**Das allgemeinste und heimtückischste Muster.** Ein Thread wurde im
`futex_wait` (cond_wait/poll) suspendiert; der Restore spült die Cond-
Buchhaltung zurück. Danach:

- Der Thread schläft im **Kernel** weiter (futex_wait bis FUTEX_WAKE).
- Jedes spätere `cond_signal` liest die **rewoundene** Waiter-Buchhaltung
  („no waiters", `__wrefs`=0) und **unterlässt den FUTEX_WAKE**.
- Ergebnis: Thread schläft für immer, obwohl Daten/Work da sind.

**Symptom:** Wartender Thread in futex_wait mit `expected=<wert>`, der
Memory-Wert an der Stelle ist <wert> (passt! „schläft legitim"), aber
Items liegen in der Queue / das Flag ist gesetzt. Kein Signal ändert
etwas.
**Diagnose:** futex-Args aus `/proc/<pid>/task/<tid>/syscall` (uaddr,
expected) gegen den Speicherwert an uaddr; u_vector head/tail; cond
`__wseq/__wrefs/__g_signals`.
**Fix-Optionen:** (a) post-restore jedem fortgesetzten Thread ein
caught-and-ignored Signal (SIGURG) geben — **FUNKTIONIERT NICHT für
cond_wait-Parker**: glibc's cond_timedwait verschluckt EINTR intern und
re-waitet, ohne zum Aufrufer-Prädikat zurückzukehren (getestet, revertet).
Nur RAW poll/read/futex-Caller kehren bei EINTR zum Prädikat zurück.
(b) mesa-seitig endliche Timeouts in den Warteschleifen (System-Mesa: nein);
(c) fundamental: **Re-Snapshot nach dem Recreate** — der Snapshot enthält
dann Thread-Positionen und Cond-State konsistent, die Lost-Signal-Lücke
zwischen Memory-Rewind und Re-Registrierung existiert nicht mehr.

**Die exakte Mechanik (Fall 4, final verifiziert):** Restore spült den
Manager zurück in seinen cond_timedwait → Destroy pusht Sentinel +
cond_signal → das Signal liest die rewoundene Waiter-Buchhaltung
(„no waiters") und unterlässt den FUTEX_WAKE → der Manager registriert
sich danach neu, aber das eine Signal ist verloren → parkt für immer
während die Queue die Items hält (gemessen: length=4, futex expected=0,
Wert bleibt 0).

### 5. restore_count/Arming-Zähler-Gating

**Symptom:** nach dem Restore fehlt die OUT_OF_DATE-Invalidation (Log:
„restore done" ohne „restore invalidated"), Acquire stallt auf der
rewoundenen Swapchain.
**Ursache:** Arming-Zähler/Flags in Codepfaden, die nur unter Bedingungen
laufen (max_restores > 0), oder Zähler-Vergleiche, die bei Multi-Restore
die Differenz-Rechnung (LAST == count-1) brechen.
**Fix:** Bool-Flag vom Restore-Pfad gesetzt, vom ersten Acquire konsumiert
(swap). Limit-Counter nur fürs Limit. **Log-Zeilen mit Zählern
kombinieren** („done=2, invalidated=1" war der sichtbare Hinweis).

### 6. Registry/Topology-Rewind

**Symptom:** „thread topology changed", „exiting untracked thread" —
Threads, die nach dem Checkpoint erzeugt wurden (jeder Recreate erzeugt
neue WSI-Threads), sind nach dem Restore unsichtbar; Exit/Suspend-Panics.
**Fix-Richtung:** recreate_missing (teilweise drin) + Exit-Check tolerant;
fundamental: Re-Snapshot nach Recreate.

## Subsystem-Checkliste bei neuen Hangs

1. **valo.log**: done vs invalidated vs kicks vs topology/panics — die
   Kombination klassifiziert meist schon.
2. **Thread-BTs** (alle, frisch): main (acquire? join? poll?), WSI q/e
   (pull? special-wait? idle-branch?), wer parkt wo.
3. **Queue-Vektoren**: head > tail = Invariantbruch (Interleaving über den
   Restore); head == tail mit parkendem Consumer = verlorener Wake.
4. **futex-Args** (`/proc/<pid>/task/<tid>/syscall`): expected-Wert gegen
   den Memory-Wert an uaddr — gleich = „schläft legitim, Wake ging
   verloren" (Fall 4); ungleich = der Thread ist gar nicht da, wo er sein
   sollte.
5. **xtrace** (`XTRACE=1 just repro`): Present-Request-Serials gegen
   IdleNotify-Serials korrelieren; Event-eids gegen die registrierten
   SelectInput-eids. Achtung: xtrace verlangsamt 2–3×.
6. **Compositor-Faktor nie vergessen**: XWayland/Satellite completet
   Presents nur bei Compositor-Fortschritt des Fensters (off-viewport =
   eingefroren). Resize/Move verändert das Verhalten — das ist
   Diagnosesignal, kein Fix.

## Meta-Lektionen der Session

- **Jeder Fix stackte den nächsten frei** — Symptome wanderten (Spin →
  Recreate bricht → Join hangt → Acquire stallt). Ein „hängt wieder"
  nach einem Fix heißt nicht „Fix war falsch", sondern „nächste Schicht".
- **Zähler-Divergenzen sind das Grundthema**: Client/Server, Kernel/
  Userspace, Snapshot/Excluded — überall wo zwei Seiten denselbenlogischen
  Stand zählen, divergieren sie durch den Restore. Die Frage ist immer:
  welche Seite ist die Wahrheit nach dem Restore?
- **Ein Commit pro validierter Schicht** — die Iteration (xm/uy/squash)
  verwässerte die Historie und beschrieb am Ende Dinge, die es so nie gab.
