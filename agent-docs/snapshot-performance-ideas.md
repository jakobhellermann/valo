<!-- // TODO(ai-review): generated, not yet audited -->

# Ideen für schnellere Checkpoints und Restores

Stand: Oktober 2026. Das ist eine Ideensammlung, keine Implementierungszusage.
Ziel ist vor allem eine kürzere Restore-Pause beim wiederholten Zurücksetzen
desselben Prozesses. Aufwand, Nutzen und Korrektheit müssen einzeln gemessen
werden.

## Ausgangspunkt

Profil mit Hollow Knight, nach 240 Frames Checkpoint und 60 weiteren Frames
Restore (`just profile`, GPU mit `VALO_USE_GPU=0` deaktiviert):

| Messwert                                                      |                                                                            Ergebnis |
| ------------------------------------------------------------- | ----------------------------------------------------------------------------------: |
| Checkpoint gesamt / Memory                                    |                                                                    707,8 / 659,7 ms |
| Restore gesamt / Memory                                       |                                                                    499,7 / 489,3 ms |
| Snapshot-`Full`-Seiten                                        |                                                                   517.573 (2,0 GiB) |
| Beim Restore clean und present, in passendem privaten Mapping |                                       112.622 (439,9 MiB; 21,8 % der `Full`-Seiten) |
| Soft-dirty, in passendem privaten Mapping                     |                                                                    314.311 (60,7 %) |
| Nicht klassifiziert                                           | 90.640 (17,5 %): 73.014 in geänderten/fehlenden Mappings, 17.626 in Shared-Mappings |

Die Dateien `/tmp/valo-snapshot-<slot>-{meta,pages}` liegen auf diesem System
bereits in `tmpfs`. „In RAM speichern“ ist daher im Wesentlichen der Ist-Zustand;
`tmpfs` kann bei Speicherdruck allerdings auslagern. Das Profil zeigt beim
Restore viel Zeit im `read`-/`shmem_file_read_iter`-Pfad, beim Checkpoint im
`write`-/`shmem_file_write_iter`-Pfad. Diese CPU-Samples sind keine direkte
Aufteilung der Wall Time.

## Naheliegender nächster Schritt

### Cleane `Full`-Seiten beim Restore überspringen

Nach dem Checkpoint setzt der Writer Soft-Dirty zurück. Beim Restore kann eine
`Full`-Seite in einem unveränderten privaten Mapping ausgelassen werden, wenn
sie noch vorhanden und nicht soft-dirty ist. Der Reader muss die Lade-Queue
vor einer Lücke leeren und die zugehörigen Snapshot-Daten im Dateistrom
überspringen. Mapping-Reallocation, Guard-Seiten und wiederholte Restores
gehören in die Korrektheitsprüfung.

Für das obige Profil sind 439,9 MiB Kandidaten. Eine grobe, **unbestätigte**
Erwartung ist eine Ersparnis im Bereich von einigen zehn Millisekunden;
eine proportionale 21,8-%-Beschleunigung des gesamten Restores wäre zu hoch
angesetzt. Vorher/Nachher mit demselben Profilablauf messen und geladene,
übersprungene und aus Sicherheitsgründen nicht übersprungene Seiten loggen.

## Größere Optionen für den Restore

### Lazy Restore mit `userfaultfd`

Nur Seiten laden, die das Spiel nach dem Resume tatsächlich berührt; die
übrigen im Hintergrund laden oder gar nicht, falls vorher wieder restauriert
wird. Das könnte die sichtbare Pause deutlich stärker verkürzen als ein
vollständiger Restore. Es verschiebt Arbeit in spätere Frames und kann dort
Page-Fault-Spitzen verursachen. Ein Pager-Worker müsste vor dem Restore
existieren, selbst vom Tracking ausgenommen sein und mit den suspendierten
Gast-Threads sowie Mapping-Änderungen koordiniert werden. Erst den tatsächlich
benötigten Seitensatz der ersten Frames messen, dann den Entwurf bewerten.

CRIU nutzt ebenfalls einen separaten `lazy-pages`-Dienst mit `userfaultfd`:
<https://www.criu.org/CLI/cmd/lazy-pages>.

### GPU- und CPU-Restore überlappen

Beim **Checkpoint** startet Valo den Kumquat-GPU-Snapshot bereits vor dem
Memory-Writer und wartet danach auf das Ergebnis. Beim **Restore** kommt
`gpu::restore()` erst nach `read_all_areas()`. Eine Aufteilung in „GPU-Restore
starten“ vor dem CPU-Restore und „auf GPU warten“ danach könnte die Pause
auf ungefähr das Maximum der beiden Zeiten statt deren Summe reduzieren.
Zuerst prüfen, ob Kumquat währenddessen Shared-Memory-Bereiche oder andere
Ressourcen verändert, die der CPU-Restore bearbeitet. GPU-Läufe mit
`VALO_USE_GPU=1` (Oktober 2026, nach der signalfd-Umbaute) messen
`restore.memory` ≈ 270–300 ms und `restore.gpu` ≈ 340–470 ms — Überlappen
würde grob den kleineren der beiden Posten sparen; sequenziell gemessen
lag restore gesamt zuletzt bei ~650 ms.

### Snapshot-Seiten direkt an Spieladressen abbilden

Ein `MAP_PRIVATE`-Mapping der Snapshot-Datei an Zieladressen würde zunächst
Seitentabellen statt Seiteninhalte ändern. Spätere Schreibzugriffe erzeugen
Copy-on-Write-Kopien. Das ist echtes Zero-Copy für die anfängliche Abbildung,
aber ein anderes Snapshot-Design: Der aktuelle Pages-Stream packt nur
`Full`-Seiten dicht zusammen; aus anonymen Spiel-Mappings würden
dateigestützte Mappings; die Snapshot-Datei müsste für wiederholte Restores
unverändert bleiben. `MAP_FIXED` und Valos Mapping-Reallocation müssten
zusammenpassen. Weiter entfernt als der Soft-Dirty-Skip.

`UFFDIO_MOVE` verschiebt vorhandene exklusive Seiten ohne Inhaltskopie,
verbraucht aber die Quelle. Ein dauerhaft wiederverwendbarer Snapshot ergibt
sich daraus allein nicht. `UFFDIO_COPY` kopiert weiterhin. Siehe
<https://man7.org/linux/man-pages/man2/UFFDIO_MOVE.2const.html> und
<https://kernel.org/doc/html/latest/admin-guide/mm/userfaultfd.html>.

### Mehrere Restore-Worker

Unabhängige Bereiche oder Seitenblöcke könnten parallel geladen werden.
Die Signal-Handler-Regel „keine Heap-Allokation“ spricht für vorab erzeugte,
vom Tracking ausgeschlossene Worker und vorbereitete Kommunikation. Ob das
hilft, hängt von Speicherbandbreite, `tmpfs`-Verhalten und der Serialisierung
durch Mapping-Reallocation ab. Erst nach dem einfachen Skip erwägen.

## Optionen für Checkpoint-Zeit und Snapshot-Größe

### Pre-Dump und inkrementelle Snapshots

CRIU kopiert bei einem Pre-Dump einen Großteil des Speichers, während die
Anwendung läuft, und erfasst beim finalen Stopp nur geänderte Seiten. Eine
ähnliche Trennung könnte Valos Checkpoint-Pause senken. Die Vorabkopie ist
allein kein konsistenter Savepoint: Seiten **und Mapping-Änderungen** zwischen
Vorabkopie und finalem Stopp müssen berücksichtigt werden. Für mehrere
Savepoints könnten spätere Images unveränderte Seiten aus einem Basis-Image
referenzieren. Das spart Checkpoint-Arbeit und RAM, verkompliziert aber
Restore und Lebensdauer der Basis-Images.

CRIU-Dokumentation: <https://www.criu.org/Memory_changes_tracking>,
<https://www.criu.org/Incremental_dumps>.

### Kompression und Deduplication

LZ4 oder ein anderes schnelles Verfahren könnte die rund 2 GiB Snapshot-Daten
verkleinern. Auf warmem `tmpfs` ist eine schnellere Wiederherstellung nicht
gesichert: Dekompression kostet CPU, und Blöcke müssen so indiziert sein,
dass saubere Seiten weiterhin übersprungen werden können. Der primäre Nutzen
wäre zunächst geringerer RAM-Bedarf und bessere persistente Savestates.
CRIU bietet LZ4-Blöcke und Deduplication zwischen inkrementellen Images;
letztere setzt einen Parent-/Delta-Aufbau voraus:
<https://github.com/checkpoint-restore/criu/blob/criu-dev/Documentation/criu.txt>,
<https://criu.org/CLI/cmd/dedup>.

### Kernel-Zero-Pages ohne Inhaltsvergleich erkennen

Mit `CAP_SYS_ADMIN` liefert `/proc/self/pagemap` die PFN einer vorhandenen
Seite. Über diese PFN kann `/proc/kpageflags` abgefragt werden. Das Flag
`ZERO_PAGE` beweist, dass die Seite die vom Kernel bereitgestellte Nullseite
ist; dafür müssen ihre 4 KiB nicht gelesen oder verglichen werden. Der Writer
könnte dann `PageFlag::Zero` statt `Full` ablegen und Snapshot-I/O sparen.
Das Flag erkennt **nicht jede** zufällig mit Nullen gefüllte reguläre Seite.

Valos `is_zero_page()` ist derzeit in Writer und Reader ein Stub. Im Writer
wird `PageFlag::Zero` bislang nur im Shared-Zweig geprüft; für private
Kernel-Zero-Pages müsste die Klassifikation dort ebenfalls greifen. Vor einem
Fix zuerst zählen, wie viele heutige `Full`-Seiten tatsächlich
Kernel-Zero-Pages sind, und die Kosten der zusätzlichen Abfragen messen.
`PAGEMAP_SCAN` bietet auch `PAGE_IS_PFNZERO`; ob das in diesem Ablauf
einfacher oder schneller ist, wäre separat zu prüfen. Siehe
<https://docs.kernel.org/admin-guide/mm/pagemap.html>.

Messung am laufenden Hollow-Knight-Prozess (PID 296238, Oktober 2026): Mit
`pkexec` waren die PFNs sichtbar. Von 357.531 vorhandenen Seiten in privaten
schreibbaren Mappings trugen **4 Seiten** `KPF_ZERO_PAGE` (16 KiB); in
Shared-Mappings und übrigen Mappings keine. Das ist ein anderer Prozesslauf
als das Profil oben. Für diesen Lauf bringt allein die Kernel-Zero-Page-
Erkennung praktisch nichts. Eine Inhaltsprüfung könnte zusätzlich reguläre,
vollständig mit Nullen gefüllte Seiten finden; deren Anzahl wurde hier nicht
gemessen.

### Write-Fault-basiertes Undo-Log

Beim Checkpoint Seiten schreibschützen, bei ihrem ersten Schreibzugriff den
alten Inhalt sichern, beim Restore nur diese Änderungen rückgängig machen.
Das könnte die anfängliche 2-GiB-Kopie vermeiden, belastet aber die laufenden
Frames durch Faults und braucht eine Lösung für Unmap, Remap, Shared Memory
und mehrere Threads. Asynchrones `userfaultfd`-Write-Protect kann **nur**
Änderungen markieren; zum Sichern des alten Inhalts vor dem Schreiben wäre
synchrone Behandlung nötig. Siehe
<https://kernel.org/doc/html/latest/admin-guide/mm/userfaultfd.html>.

## Ansätze mit derzeit geringer Priorität

- **`mmap` als bloße Alternative zu `read`:** Die Daten müssen weiterhin in
  Spielseiten kopiert werden. Der im Profil sichtbare `_copy_to_iter`-Aufwand
  würde größtenteils zu `memcpy` wandern; dazu kämen Page Faults und ein
  Snapshot-Mapping, das Valo tracken und beim Restore erhalten müsste.
- **`ramfs` statt `tmpfs`:** verhindert Swap, entfernt aber keine Kopie. Erst
  relevant, falls Auslagerung des Snapshot-Files gemessen wird.
- **`madvise`/`posix_fadvise`:** kann Seiten vorladen oder Zugriffsmuster
  mitteilen, spart aber keine Kopie. Nach einem Checkpoint sind die
  `tmpfs`-Seiten üblicherweise bereits im Cache, solange kein Speicherdruck
  sie verdrängt hat.
- **`vmsplice`/Page-Pipe wie bei CRIU:** betrifft den Writer. Ohne echtes
  Verschenken der Spielseiten bleibt die Kopie in die Snapshot-Datei; Pipe-
  Verwaltung kommt hinzu. `SPLICE_F_GIFT` passt nicht zu Speicher, den das
  Spiel später weiterverwenden muss. Siehe
  <https://man7.org/linux/man-pages/man2/vmsplice.2.html>.
- **PFN-Vergleich beim Restore:** Eine PFN erlaubt kein beliebiges Umhängen
  physischer Seiten. Dieselbe PFN beweist ohne Dirty-Tracking keinen
  unveränderten Inhalt. Für den Restore-Skip genügen die heute lesbaren
  Present- und Soft-Dirty-Informationen. Der sinnvolle PFN-Anwendungsfall
  hier ist die Zero-Page-Erkennung **beim Checkpoint** (oben).
- **`PAGEMAP_SCAN` und asynchrones `userfaultfd`-Write-Protect:** Mögliche
  spätere Alternative zu Soft-Dirty für effizienteres Tracking. Das ist ein
  anderes Tracking-Protokoll, kein unmittelbarer Zero-Copy-Restore. Siehe
  <https://docs.kernel.org/admin-guide/mm/pagemap.html>.

## Reihenfolge für Experimente

1. Soft-Dirty-Skip einbauen, Korrektheit über mehrere Restore-Zyklen prüfen,
   Zeit und tatsächlich ausgelassene Bytes im Log/Visualizer zeigen.
2. Mit GPU aktiv CPU-/GPU-Phasen separat messen; nur bei relevantem GPU-Anteil
   die Restore-Überlappung und ihre Shared-Memory-Abhängigkeiten untersuchen.
3. Für Lazy Restore erfassen, wie viele Seiten die ersten Frames nach dem
   Resume berühren. Bei kleinem Working Set einen Pager-Prototyp erwägen.
4. Checkpoint-Pause separat angehen: Pre-Dump oder inkrementelle Images nur
   dann priorisieren, wenn häufige neue Savepoints gebraucht werden.
5. Für Zero-Page-Erkennung zuerst die Trefferquote auf echten Profilen
   messen; bei vielen Treffern PFN/`kpageflags` und `PAGEMAP_SCAN` vergleichen.
