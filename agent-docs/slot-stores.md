<!-- // TODO(ai-review): generated, not yet audited -->

# Savestate-Slot-Stores: Backing, Window-Pipeline und der Folio-Kompromiss

Stand: Oktober 2026. Zustand und Erkenntnisse nach dem Store-Neubau
(`checkpoint::store`, Commit „savestate slot stores with memfd or disk
backing") inklusive der Messungen, die zur geplanten
MADV_COLLAPSE-Pipeline führen. Verwandt:
`agent-docs/snapshot-performance-ideas.md` (Restore-Seite),
`docs/signal-state.md` (Handler-Kontexte).

## Was existiert

**Ein Store pro Slot** (`checkpoint/store.rs`), angelegt beim ersten
Checkpoint des Slots und für die Prozesslebensdauer von der Lib offen
gehalten (bei memfd ist der Descriptor die einzige Referenz auf die
Daten). Registry in `ReservedMemory` (rewind-sicher), fds per
`fcntl_dupfd_cloexec` ab 1024 (geteilter Nummernraum mit den
Keepalives), von `fd_sync` als Host-State ausgenommen — weder als
Guest-Deskriptoren aufgenommen noch beim Restore geschlossen.

**Backing ist Config** (`valo_ipc::config::SavestateConfig::Backing`):
`Memfd` (Default, shmem ohne Ort, keine Mount-Anforderungen) oder `Disk`
(`pages`-Datei im Slot-Dir unter `VALO_HOME`; für echte Messungen
`VALO_HOME` auf einen Datenträger-Pfad zeigen — `scripts/profile`
pinned es im Disk-Modus automatisch ins State-Dir). Konsultiert wird die
Config nur bei Slot-Anlage; Slots behalten ihr Backing über
Config-Wechsel (auch zur Laufzeit, an Frame-Boundaries).

**Schreibpfad heute: ephemere Window-Mapping.** Kein `write(2)`, kein
Preallocate, kein SLOT_CAP mehr: pro Checkpoint mappt `prepare` (normaler
Kontext, nach `suspend_others`) ein `MAP_SHARED`+`MADV_HUGEPAGE`-Window
(1 TiB VA, Datei wächst per `ftruncate` in 256-MiB-Schritten vor dem
Cursor), der Handler memcpys hinein, danach wird das Window gemunmapt.
Restore preadet vom Registry-fd, berührt kein Mapping. Regressionstest
`savestate_recheck` deckt Re-Checkpoint nach Restore ab.

**Vergleichswerkzeug:** `just profile BACKING=<memfd|disk>` bzw.
`just profile-compare` (je ein Lauf pro Backing, Timings in
`out/perf-timings-<backing>.txt`, History in `out/perf-history.tsv` mit
Backing in der mode-Spalte).

## Die Messlage — drei Regime, eine_pipeline-Wahrheit

Alle Zahlen Hollow Knight; „Frame 240" = `just profile`-Defaults, „Session"
= eigener Spielstand mit 1,6 GiB Full-Page-Daten (~432k Seiten).

| Messung | Zeit | Regime |
| --- | --- | --- |
| write(2)-Pfad (alt), Session | CP 501 ms | druckunabhängig (order-0) |
| write(2)-Pfad (alt), Frame 240 | CP 606 ms | siehe snapshot-performance-ideas |
| Prototyp-Windows (tmpfs), Frame 240 | CP 349 ms | ungedrückt |
| Store-Neubau memfd, Frame 240 | CP 290 ms | ungedrückt (21:00) |
| Store-Neubau memfd, Frame 240 | CP 1103 ms | unter Druck (21:45) |
| Store-Neubau memfd, Session | CP 1883 ms | unter Druck |
| Restore, alle Varianten | 270–315 ms | kaum empfindlich |

**Warum der Wechsel zwischen 290 und 1103 ms bei gleichem Code:** die
Window-Pipeline zahlt pro 2-MiB-Folio im Fault. Ungedrückt ist das
Allokation+Nullung (~7 GB/s, immer noch schneller als write(2)s ~4,3).
Unter Druck (freie Order-9-Blöcke unter der Watermark) geht der Fault in
direkte Compaction: perf auf dem 1103-ms-Lauf zeigt 44 % `migrate_pages`/
Compaction *im Checkpoint-Fenster, auf unserem Thread* — plus ~12 %
`clear_highpages` (Folio-Nullung, die write(2) vollständiger Seiten
überspringen kann). Die Fault-Zeit sitzt unter `do_user_addr_fault`;
**Prefault in `prepare` bringt deshalb nichts** — derselbe Thread, dieselben
Faults, nur früher.

write(2) ist flach, weil order-0 nicht compactet. Der gemessene
„Fortschritt" der Window-Pipeline existiert also nur im ungedrückten Regime;
der alte Prototyp-Vergleich (606→349) wurde in genau dem Regime gemacht und
sagt nichts über Druckzustände aus. Achtung bei System-Knobs:
`defrag=defer+madvise` lässt den Huge-Fault schnell auf 4 KiB fallen — und
4-KiB-Fault-memcpy ist mit ~2,3 GB/s die *schlechteste* Variante überhaupt
(gemessen). Die Sysfs-Lösung passt nur zu einer Pipeline ohne Fault-in-
Window, nicht zur aktuellen.

**Buddy-Check:** „haben wir fragmentiert?" — nein: 1235 freie Order-9-
Blöcke im Nachhinein. Es ist Burst-Bedarf gegen Watermarks, keine
dauerhafte Fragmentierung; der Zustand driftet mit der Sitzung.

## Der Plan: write(2) zurück, MADV_COLLAPSE als Nachgang

Robust über beide Regime ist nur eine Schreibseite ohne Huge-Fault:

1. **Writer zurück auf `write(2)`** in den Store (druckstabile ~460-ms-
   Klasse bei 1,6 GiB; 4-KiB-Folios entstehen, Restore liest dann
   zunächst Alt-Rate ~7 GB/s).
2. **`MADV_COLLAPSE` nach dem Resume**, asynchron auf einem valo-Thread,
   in 2-MiB-Schritten über `[0, pages_cursor)`: konvertiert vorhandene
   Folios synchron zu 2 MiB (khugepaged-Arbeit auf Zuruf; ~6 GB/s, gemessen
   40–48 ms/256 MiB), Einzelfehler (ENOMEM unter Druck) ignorierbar — der
   Chunk bleibt 4 KiB, Restore liest einfach langsamer. Berührt nur Host-
   State, ist also determinismussicher vom kritischen Pfad entkoppelt.
   Koordination: der nächste Checkpoint desselben Slots wartet auf das
   Ende oder bricht es ab.
3. **Finalize-Shrink streichen (High-Water-Slots).** Der Shrink gibt nur
   den Schwanz jenseits des Cursors frei, zwingt aber jeden Re-Checkpoint
   zur Neu-Allokation (bzw. zerstört kollapste Folios — Collapse wäre
   umsonst). Ohne Shrink trifft der zweite Checkpoint auf existierende
   Folios: write(2) in vorhandene Seiten ist reine Kopie, Restore liest
   Huge-Folios weiter. Kostet Quota (High-Water statt Ist-Größe), auf
   memfd nur memcg — akzeptiert.
4. **Restore bleibt unangetastet** (pread vom Registry-fd); die bekannten
   Reader-Hebel stehen weiter in snapshot-performance-ideas.md.

Offene Punkte beim Umbau: `savestate_recheck`-Kommentar anpassen (Grow über
altes EOF wird durch Folio-Wiederverwendung ersetzt), Collapse-Job-Lebens-
zyklus über Restores (Registry in ReservedMemory), und ob der
Erscheckpoint-`write(2)`-Cursor anders allokieren soll als der alte Pfad.

## Metrische Fußangelen dieser Session (kurz, damit sie niemand wiedertritt)

- **Gemeinsamer `CARGO_TARGET_DIR` vergiftet Workspaces gegenseitig**:
  ein Build im zweiten jj-Workspace überschreibt das valo-Binary; der
  Haupt-Workspace hält es für frisch (mtime) → Stale-Binary, z. B.
  „postcard decode: Found a bool that wasn't 0 or 1". Reparatur: Quellen
  `touch`en.
- `pkill -f <pattern>` matcht die eigene bash-Kommandozeile → Shell
  killt sich selbst; `[h]`-Bracket-Trick.
- `scripts/profile`-GAME-Default war ein toter Pfad (`~/.steamapps`); der
  Heroic-Install hat `UnityPlayer_s.debug` und damit funktionierende
  Symbol-Auflösung (27 Player-Symbole). Fallback-Kette jetzt wie im
  Justfile.
- Der Spawn-Pfad löste früher keine Unity-Symbole auf (`msg::spawn`
  anders als `run`): die Gamespecific-Hooks installierten still nicht; seit
  dem Fix „resolve unity player symbols on the spawn path". Der IPC-Commit
  „keep the msg listener reachable during lib init" gehört kausal dazu
  (Listener-Bind vor Hook-Install; `ConnectionRefused`-Retry im
  connect_stream).
