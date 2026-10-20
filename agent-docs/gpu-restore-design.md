# GPU-Restore: Redesign und Begründung

Stand: 2026-10-21. Ersetzt den Reattach-Ansatz der alten `wip`-Branches
(gfxstream, rutabaga_gfx, mesa; heute nur noch als nicht verfolgtes Backup
vorhanden) und die Problemliste in gfxstreams `NEXT_STEP.md`.

## Begriffe

- **Spiel/Gast**: der Prozess unter valo, mit dem gfxstream-Vulkan-ICD aus
  mesa.
- **kumquat**: virtio-gpu-Server aus rutabaga_gfx; enthält rutabaga und
  gfxstream (Host-Renderer auf dem echten Treiber, RADV oder anv). Läuft als
  eigener Prozess, gestartet vom valo-Client.
- **GPU-Host-Objekte**: Speicher, den gfxstream/RADV im kumquat-Prozess
  anlegt und dem Spiel als fd gibt:
  - ASG-Ring und Transferpuffer (memfd), pro Kontext;
  - `HOST_VISIBLE`-Allokationen des Spiels (virtio-Blobs, dma-buf von RADV,
    mit ReBAR auch VRAM);
  - exportierte Images (dma-buf, etwa für die Swapchain).
- Vulkan `HOST_VISIBLE` heißt CPU-mappbar, virtio `HOST_VISIBLE` heißt auf
  der Host-Seite angelegt und für den Gast mappbar. Nicht verwechseln.

## Ausgangslage

Der alte Ansatz hielt den Gast-Speicher für GPU-Objekte über Restores hinweg
fest und hängte die Host-Seite wieder an ihn an („Reattach"):

- gfxstream bekam `reattach_*`-APIs, eine Ring-Tabelle im
  ExternalObjectManager, einen shmem-Fallback, eine getauschte
  Restore-Reihenfolge und Sonderzweige für Guest-Handles und Ringe.
- kumquat führte fd-Tabellen pro Slot, memoisierte Mappings, Archive für
  verworfene Ressourcen, einen Write-Guard, Epochen und eine Reattach-Schleife;
  Guest-Blobs kamen über udmabuf (`CREATE_GUEST_HANDLE`, dafür auch ein
  mesa-Patch).
- valo sicherte Ring und Guest-Blobs zusätzlich selbst (bei HK 70–90 MB pro
  Slot) und ging davon aus, dass valo und kumquat parallel dieselben Bytes
  schreiben.
- Gesteuert wurde über Signale (SIGUSR1/2, `sigqueue`), `KUMQUAT_PID`, eine
  geerbte Ergebnis-Pipe (`VALO_GPU_RESULT_FD`) und Polling auf den Socket.

Das hatte drei grundsätzliche Probleme:

1. **Slots.** Ein Objekt, das in der aktuellen Zeitlinie zerstört ist, wird
   von einem älteren Slot wieder gebraucht. Daraus folgten Archive, Epochen
   und fd-Tabellen pro Slot, also eine ganze Problemklasse „jetzt tot, später
   gebraucht".
2. **Upstream-Ferne.** Die Reattach-APIs gibt es nur für diesen Fall; kein
   VMM braucht sie, entsprechend schlecht sind sie upstream zu begründen.
3. **Steam.** Geerbte fds und Signale überleben die Kette aus
   steam-launch-wrapper, soldier und scout nicht.

## Grundsatz

**Aller GPU-Speicher, den das Spiel sieht, wird von gfxstream angelegt.** Es
gibt keine Guest-Handles. **Restore läuft wie bei crosvm:** gfxstream legt
seine Objekte neu an und füllt sie aus dem eigenen Snapshot; die VMM-Rolle
hängt sie danach an die alten Gast-Adressen. Bei uns ist die VMM-Rolle
geteilt: kumquat liefert die neuen fds, valo hängt sie im Spiel um.

Begründung:

- **Es ist der vorhandene Upstream-Pfad.** crosvm stellt in genau dieser
  Reihenfolge wieder her (`rutabaga.restore` → `attach_backing` →
  `resource_map_blob`, Gerät schläft). Für den Speicherpfad muss gfxstream
  nichts Neues lernen.
- **Slots werden trivial.** Jeder Restore erzeugt alles neu aus dem Snapshot
  des Ziel-Slots. Was die aktuelle Zeitlinie inzwischen zerstört hat, spielt
  keine Rolle mehr; die Problemklasse aus Punkt 1 verschwindet.
- **valo bleibt beim 1:1-Modell** (Inode-Identität, Keepalives) für alles
  andere. GPU-Host-Objekte sind ein klar abgegrenzter Sonderfall, den valo
  auslässt und am Ende umhängt.
- **Nichts wird doppelt gesichert.** Den Inhalt sichert gfxstream über seinen
  Vulkan-Snapshot; valo spart die RAM-Seiten dieser Objekte.

## Ablauf

**Checkpoint (Slot N)**

1. valo friert das Spiel ein.
2. valo fragt per `LIST` die Blob-Ressourcen ab, deren Speicher ein Client
   hält. kumquat antwortet mit `RESP_OK`, je Ressource `RESOURCE`
   (`u32` resource_id, `u64` dev, `u64` ino) und `END`. valo sucht die
   zugehörigen Mappings in `/proc/self/maps`. Liste und Bereiche liegen in
   Speicher, den der Restore zurückspult, gehören also automatisch zum Slot.
3. valo schickt `SNAPSHOT <home>/slots/<N>/gpu`. kumquat ruft
   `rutabaga.snapshot` auf (gfxstream sichert Vulkan-Zustand und Inhalt:
   Ring als Bytes, host-sichtbaren Speicher über gebundene Buffer und Images
   per GPU-Kopie, dazu die Command-Buffer-Aufzeichnungen) und schreibt
   `kumquat.json` mit seiner eigenen Ressourcen-Buchhaltung. Antwort
   `RESP_OK` oder `RESP_ERR` mit Fehlertext.
4. valo lässt die gelisteten Objekte beim Sichern aus: kein Keepalive in
   `fd_sync`, kein Inhalt beim Speicher-Dump (`AreaMeta.gpu`).

**Restore (Slot N)**, GPU- und Speicher-Restore laufen parallel

1. valo schickt `RESTORE <home>/slots/<N>/gpu` und stellt parallel den
   Speicher wieder her; GPU-Host-Objekte fasst es dabei nicht an.
2. kumquat: `rutabaga.restore`, Backings neu anhängen, jede Ressource neu
   exportieren. Antwort `RESP_OK`, je Ressource `REMAP` (resource_id, fd des
   neuen Speichers per `SCM_RIGHTS`), dann `END`.
3. valo nach `fd_sync`, vor dem Fortsetzen der Threads: jedes neue Objekt per
   `mmap(MAP_SHARED|MAP_FIXED, Offset 0)` an die gespeicherten Adressen und
   per `dup3` auf die gespeicherten fd-Nummern mit den gespeicherten Flags.
   Liegt der empfangene fd schon auf der Zielnummer (fd_sync hat sie gerade
   freigegeben), setzt valo nur die Flags per `fcntl`. Fehlt eine Ressource
   oder schlägt ein Schritt fehl, bricht der Restore laut ab.
4. Swapchain: valo lässt den nächsten Acquire mit OUT_OF_DATE antworten; das
   Spiel baut die Swapchain neu, der X-Server bekommt neue dma-bufs.

## Steuerverbindung

Ein `SOCK_SEQPACKET`-Socketpair zwischen libvalo und kumquat. Jede Nachricht
beginnt mit einem `u32`-Typ (little endian), danach die Nutzlast; fds hängen
per `SCM_RIGHTS` an.

- Der valo-Client erzeugt das Paar, startet kumquat als direktes Kind mit
  `--control-fd` und schickt das andere Ende per `SCM_RIGHTS` über die
  bestehende valo-IPC (`VALO_SOCK`, `ClientToLib::GpuControl`) an libvalo.
- kumquat meldet `READY`, sobald der GPU-Socket gebunden ist, und beendet
  sich, wenn der Elternprozess die Verbindung schließt.
- Ersetzt: `KUMQUAT_PID`, `VALO_GPU_RESULT_FD`, SIGUSR1/2 mit `sigqueue`,
  die Ergebnis-Pipe und das Socket-Polling beim Start.

Warum ein eigener Kanal und nicht das Client-Protokoll:

- **Rolle.** Das Client-Protokoll ist die Geräteschnittstelle des Gast-ICD,
  das Gegenstück zu virtio-gpu-Befehlen. Snapshot und Restore betreffen die
  ganze Instanz, alle Kontexte und Clients; bei crosvm ist das Aufgabe des
  VMM über dessen eigenen Steuerkanal.
- **Was der alte Pfad nicht kann.** Upstream hat `SnapshotSave`/
  `SnapshotRestore` im Client-Protokoll, genutzt von gfxstreams
  End-to-End-Tests (`tests/end2end/kumquat_instance.cpp`), in denen der
  Testprozess Gast und Steuerer zugleich ist. Es gibt kein Zielverzeichnis
  (fest `/tmp/`), keine Objektliste, keine Rückgabe neuer fds und keine
  Fertig-Meldung vor dem ersten Client.
- **Umfang.** Das Client-Protokoll liegt in mesa
  (`magma-gpu-rs/lib/protocols/kumquat_gpu_protocol`) und als vendorte Kopie
  in rutabaga. Eine Erweiterung müsste in mesa, rutabaga und den
  gfxstream-Tests zugleich nachgezogen werden; die Steuerverbindung ändert
  nur kumquat.
- **Steam.** Der Pfad zum valo-IPC-Socket überlebt die Wrapper-Kette,
  geerbte fds und Signale nicht.

Die alten Befehle bleiben unverändert. Ob upstream den separaten Kanal
annimmt, ist offen (Frage an upstream steht aus).

## Identität der Objekte

kumquat meldet pro Ressource `(dev, ino)` des exportierten Speichers. Der
Prototyp hatte Objekte über Namen erkannt (`DMA_BUF_SET_NAME
kumquat-res-<id>`, gfxstreams memfd-Namen des Rings). Inode-Identität ist
das, womit valo ohnehin jeden fd und jedes Mapping vergleicht, braucht keine
Konvention zwischen den Prozessen und ist eindeutig, auch bei gleichnamigen
Objekten.

## Was in welchem Repo geändert ist

Alle Repos frisch ab `main`, mesa auf `gfxstream-kumquat-socket-env`.

**rutabaga_gfx**

- `build.rs`: mit eigenem gfxstream-Pfad (`GFXSTREAM_PATH*`) liest es die
  pkg-config-Datei nicht, in der der Define `GFXSTREAM_UNSTABLE` steht;
  `GFXSTREAM_UNSTABLE=1` ergänzt ihn. Ohne das Flag fehlen Fence-Export,
  Snapshot und Restore (Symptome: „unable to create host sync object",
  „snapshot failed: … not implemented").
- `rutabaga_gfx: re-export blob handles after restore`: `Rutabaga::restore`
  ließ die Blob-Handles leer, danach schlug `export_blob` fehl. Das trifft
  crosvms `resource_map_blob` nach einem Restore genauso; die Komponente
  exportiert jetzt neu (`export_restored_blob`, `handed_out` für Handles, die
  `export_blob` schon herausgegeben hat).
- `rutabaga_gfx: allow snapshotting over an older snapshot`: ein Slot wird
  überschrieben, statt am bestehenden Verzeichnis zu scheitern.
- `kumquat: print log records to stderr`, `kumquat: route renderer messages
  through the logger`: gfxstream schrieb Info-Meldungen auf C-stdout, das bei
  Umleitung in eine Datei blockweise gepuffert ist und beim Abbruch verloren
  geht. Jetzt laufen alle Meldungen mit Level ungepuffert über stderr.
- `kumquat: close a failing client connection instead of exiting`: ein
  fehlerhafter Client beendet nicht mehr den ganzen Server.
- `kumquat: add a control connection for the parent process`,
  `kumquat: restore like a VMM and hand out the new blob memory`: siehe oben.

**gfxstream**

Für den Speicherpfad nichts. Dazu kommen Fixes am Vulkan-Snapshot, die HK
brauchte, jeder mit eigenem Befund:

- `meson: add an option to build snapshot support`: Meson baut mit
  `-Dsnapshot=enabled` die Snapshot-Unterstützung samt Protobuf-Dateien.
  Bazels Standardkonfiguration ist der Android-Host-Fall und setzt
  `GFXSTREAM_UNSTABLE_VULKAN_BLOB_COLOR_BUFFER`/`_EXTERNAL_SYNC` nicht, ohne
  die `vkCreateSwapchainKHR` unter kumquat scheitert.
- `host: snapshot Buffer objects`: `FrameBuffer` sicherte `m_buffers` nicht;
  nach dem Restore fehlten Buffer-Ressourcen.
- `vulkan-snapshot: allocate batched descriptor sets on load`: mit gebatchten
  Descriptor-Updates legt der Gast Sets erst beim Commit an, den der Load
  nicht abspielt; der Load legt die gesicherten Sets jetzt selbst an.
- `vulkan-snapshot: drop descriptor writes of views whose image is
  destroyed`: gespeicherte Writes zeigten auf Views zerstörter Images.
- `vulkan-snapshot: read back saved content through cached memory`: das
  Zurücklesen aus write-combined Speicher war um Größenordnungen langsamer
  (GPU-Snapshot bei HK 2481 → 356 ms).
- `vulkan-snapshot: restore command buffer recordings`: siehe nächster
  Abschnitt.
- `vulkan-snapshot: share staging per device and copy each image in one
  submit on save` / `… on load`: ein Staging-Kontext pro Device und ein
  Submit pro Image statt Allokation und Submit pro Mip-Level und Layer.
  Gemessen mit HK (je drei Läufe): Checkpoint ~790 → ~637 ms, Restore
  ~360 → ~245 ms.

**valo**

- `ipc: hand the GPU host's control socket to the lib`,
  `gpu: drive kumquat over its control connection`,
  `gpu: replace the GPU host's objects after a restore`: Steuerverbindung,
  Auslassen der GPU-Host-Objekte, Umhängen nach `fd_sync`. Laut scheitern ein
  Shared-Mapping von `/dev/dri/*` im Spiel (Checkpoint), ein `REMAP` für eine
  Ressource, die das Spiel nicht hielt, und abgeschnittene Nachrichten auf der
  Steuerverbindung.
- `gpu: log GPU host descriptors and their remaps for the visualizer`:
  Events `checkpoint.fds.gpu` und `restore.fds.remap`; der Visualizer zeigt
  pro fd die Restore-Aktion und markiert nur Abweichungen rot, die keine
  Aktion erklärt. Neue Inodes umgehängter Objekte sind gewollt.


**mesa**

- `gfxstream: kumquat: map dma-buf blobs directly`: siehe „Kein GPU-Treiber
  im Spielprozess". Der kumquat-Client braucht kein `CREATE_GUEST_HANDLE`.

## Command-Buffer-Aufzeichnungen

**Befund.** Nach dem Restore stürzte kumquat in RADVs `vkQueueSubmit` ab,
sobald das Spiel einen vor dem Checkpoint aufgezeichneten Command Buffer
erneut abschickte. Der Snapshot stellte die Command Buffer im Initialzustand
wieder her; ihr Inhalt fehlte. Roter Test: `gpu_cmdbuf_resubmit_restore`.

**Lösung.** Der Gast-ICD schickt Command-Buffer-Inhalte unter virtio-gpu
immer als deferred commands: `vkBeginCommandBuffer` setzt den Staging-Stream
zurück, beim Submit flusht `vkQueueFlushCommandsGOOGLE` den Stream, und der
Host dekodiert ihn mit `subDecode`. gfxstream merkt sich pro Command Buffer
die seit dem letzten Reset geflushten Bytes und die boxed Handles, die das
Dekodieren entpackt hat (`ScopedUnboxedHandleRecorder`). Beim Load spielt es
sie über denselben `subDecode`-Pfad ab, Secondaries vor Primaries.

Begründung:

- **Derselbe Pfad wie live.** Das Abspielen baut auch gfxstreams Buchhaltung
  pro Command Buffer neu auf (Image-Layouts, Color-Buffer-Acquire/Release,
  `subCmds`) und schneidet beim Abspielen gleich wieder mit, sodass auch der
  nächste Restore funktioniert.
- **Kein Umbau an Codegen, Paketen oder Replay-Graph.** Der Fork hatte die
  Aufzeichnungen in den Replay-Graphen eingehängt und dafür Abhängigkeiten,
  Zeitstempel und Sonderfälle gebraucht.
- **Ungültige Aufzeichnungen werden laut verworfen.** Nutzt eine Aufzeichnung
  ein zerstörtes Objekt oder führt sie einen Secondary ohne Aufzeichnung
  aus, ist sie laut Spec ungültig; der Gast kann sie nicht erneut abschicken.

Dazu setzt `vkResetCommandPool` jetzt die Infos seiner Command Buffer zurück,
was vorher fehlte.

## Kein GPU-Treiber im Spielprozess

**Befund.** Auf anv drehte sich vkcube nach dem Restore nicht mehr; das Spiel
lief weiter, aber die GPU sah die CPU-Schreibzugriffe nicht
(`gpu_host_visible_remap`: `cpu: CORRUPTED` statt `PRESERVED`). Der
kumquat-Client in mesa (`VirtGpuKumquatBlob.cpp`, `createMapping`) importierte
jeden Blob mit Vulkan-Info über `GfxStreamVulkanMapper` in einen echten
Treiber im Spielprozess und mappte ihn mit `vkMapMemory`. Gewählt wurde das
über `vulkan_info.device_id.device_uuid[0] != 0`; RADVs UUID beginnt mit der
PCI-Domain 0, also mappte AMD die dma-buf zufällig direkt, anv nahm den
Mapper. Dessen Mappings zeigen in `/proc/maps` die Render-Node statt der
dma-buf-Inode; valo fand kein Objekt dazu, ersetzte nur die fds und sicherte
die Bereiche als Datei-Mappings, an denen ab dem zweiten Restore die
Neuanlage scheiterte.

**Lösung.** Der Client mappt dma-bufs direkt; nur andere Handle-Typen (opaque
fd, Windows, Apple) gehen über den Mapper. `resource_info` liefert dafür den
Handle-Typ. Unter Linux exportiert gfxstream dma-bufs, sobald der Treiber es
kann (`vk_decoder_global_state.cpp`), also mappt der Client auf RADV und anv
direkt, und valos Inode-Identität trifft.

Begründung:

- **Kein zurückgespulter Treiberzustand.** Ein Treiber im Spielprozess hätte
  Zustand im zurückgespulten Speicher, aber seine Kernel-Seite (DRM-fd,
  GEM-Handles) nicht. Ohne Treiber gibt es diesen Fall nicht.
- **Ein Weg für Mappings.** valo erkennt GPU-Host-Objekte weiter allein an der
  Inode; es braucht keine Adressmeldung des Clients (ein Prototyp mit
  `RESOURCE_MAP_BLOB` war verworfen).
- **Gleichwertig gemessen.** Auf anv treffen `mmap` der dma-buf und
  `vkMapMemory` dieselbe GEM-Abbildung (gleicher DRM-Fake-Offset, dieselben
  Seiten) mit gleichem Durchsatz.

Abweichung vom Kernel-Vertrag: laut `drivers/dma-buf/dma-buf.c` (DOC: cpu
access) muss CPU-Zugriff auf eine gemappte dma-buf mit `DMA_BUF_IOCTL_SYNC`
geklammert werden; Vulkans `HOST_COHERENT` kennt das nicht. Auf x86 mit xe und
amdgpu reicht das gleiche Mapping wie beim Treiber; auf nicht kohärenten
Systemen wäre der Pfad zu prüfen. crosvm nimmt bei Vulkan-Info den
Vulkan-Pfad, mappt aber im VMM und nicht im Gast.

## Unterwegs gefunden, unabhängig vom Design

**Xlib-Socket nach dem Restore** (valo, `xlib: take the socket from xcb again
after a restore`, Test `xlib_sequence_restore`). Xlib und xcb teilen sich
einen Socket; Xlib holt sich per `xcb_take_socket` das Schreibrecht und
merkt sich in `last_flushed`, wie viele Anfragen es xcb gemeldet hat. Beim
Restore kam `last_flushed` aus dem Checkpoint, während valo `request` auf den
Live-Wert setzte. Xlibs erster Flush meldete xcb deshalb den ganzen Vorsprung
der Live-Zeitlinie als gesendet (bei HK bis zu 137966 Anfragen, die es nie
gab). xcbs Sequenznummern liefen dem Server davon, Replies roher xcb-Nutzer
wie des Vulkan-WSI fanden ihren Wartenden nicht mehr, und HK hing nach vielen
Loads. Jetzt gibt valo das Schreibrecht beim Restore auf beiden Seiten frei;
Xlibs nächste Anfrage holt es neu und übernimmt xcbs Live-Zähler. libTAS hat
dieselbe Lücke (`Checkpoint.cpp`); sie trifft dort nur Code, der rohes xcb
auf der Xlib-Verbindung nutzt und auf Replies wartet.

**`vk_present_restore`** war nur am Snapshot rot: ein Window-Manager
konfigurierte das frisch gemappte Fenster um und erzeugte ein OUT_OF_DATE
vor dem Checkpoint. Der Test nutzt jetzt ein `override_redirect`-Fenster.

**`gpu_host_visible_flush_invalidate`** prüft Kopiermodus-Semantik (Invalidate holt
überschriebene Bytes zurück) und kann mit geteilten Mappings nur
fehlschlagen. Er liegt mit dem Kopiermodus auf `copy-mode`.

## Dem Design inhärente Einschränkungen

1. Ein neues Protokoll und ein Sonderfall in valo an heikler Stelle
   (Checkpoint, `fd_sync`, Speicher-Restore).
2. Das Spiel bekommt neue Kernel-Objekte: gleiche Adresse und fd-Nummer,
   anderer Inode. Gemerkte Identität oder an andere Prozesse weitergegebene
   fds folgen nicht. Die Swapchain ist über OUT_OF_DATE abgedeckt, andere
   Weitergaben (Compositor, Video, Kindprozess) nicht.
3. Der Inhalt hängt an gfxstreams Snapshot. Host-sichtbarer Speicher wird nur
   über gebundene Buffer und Images gesichert; die Kosten verschieben sich von
   valo (RAM-Seiten) zu gfxstream (GPU-Kopie).
4. Offsets innerhalb von dma-bufs sind aus `/proc` nicht ablesbar
   (DRM-Fake-Offset). Invariante: der kumquat-Client mappt ganze Objekte ab
   Offset 0; sonst müsste er den Offset melden.
5. Das Umhängen läuft seriell nach GPU- und Speicher-Restore (~0,7 ms).
6. Das Spiel bekommt die Memory-Types des Host-Treibers direkt. Da gfxstream
   sie exportierbar anlegt, sind sie auf anv auch bei `HOST_CACHED`
   write-combined (`gpu-host-visible-slow-reads.md`).
7. Ein Restore setzt ganz kumquat zurück: ein Gastprozess pro kumquat.
8. Im Spielprozess darf kein GPU-Treiber Speicher mappen; der Checkpoint
   bricht sonst ab (siehe „Kein GPU-Treiber im Spielprozess").

Nicht verbaut: ein kalter Restore (gleicher Lookup, Mappings aus den
gespeicherten statt den aktuellen Bereichen) und der WSL-Kopiermodus (dort
ist nur der Ring umzuhängen).

## Nachweis

- `gpu_host_visible_remap`: 1 Slot, 2 Restores. CPU-Mapping und GPU zeigen
  den Checkpoint-Inhalt, eine CPU-Schreibung nach dem Restore erreicht die
  GPU, es ist also dasselbe Objekt. Ohne Umhängen hängt der Ring.
- `gpu_host_visible_multislot`: 2 Slots, 6 Restores, ein Buffer zwischen den
  Snapshots angelegt, kommt mit jedem Restore des jüngeren Slots zurück.
- `gpu_cmdbuf_resubmit_restore`, `xlib_sequence_restore`,
  `xcb_slot_sequence_restore`, `vk_present_restore`.
- valo-Suite vollständig grün (59 von 59).
- anv (Arc B390): `gpu_host_visible_remap` vor dem mesa-Fix rot, danach grün;
  mit der alten Weiche bricht der Checkpoint am Render-Node-Check ab. vkcube:
  kein `renderD128`-Mapping und kein anv im Spiel, die gemappten Objekte
  liegen nach dem Restore auf neuen Inodes an denselben Adressen.
- HK: Checkpoint und Restore im Spiel über viele Loads und mehrere Slots ohne
  Hänger, nach dem Xlib-Fix.

## Offen

- Langsame CPU-Reads aus host-visible Speicher auf anv: exportierbare BOs
  sind write-combined, auch bei `HOST_CACHED`. Siehe
  `gpu-host-visible-slow-reads.md`.
- Silksong-Hänger beim Checkpoint, einmal gesehen: kumquat wartete in
  `saveBufferContent` auf den Fence der Snapshot-Kopie, ohne GPU-Reset im
  Kernel-Log. Vermutung: ein Submit des Spiels steht vor der Kopie auf
  derselben Queue und wartet auf eine Semaphore, die nie signalisiert wird.
  Auf `dev` in gfxstream liegt dafür eine Diagnose (`wip: report pending
  submits when a snapshot transfer stalls`): nach 500 ms Wartezeit loggt
  kumquat die letzten Submits mit Semaphoren-Zustand und markiert verworfene
  Aufzeichnungen.
- Silksong-Hänger beim Restore, einmal gesehen: ein Thread änderte während
  `freeze_lifecycle` seinen Lebenszyklus und lief in `abort!("pthread
  lifecycle changed during snapshot")`. Nicht reproduziert; Fix-Idee: die
  Operation warten lassen statt abbrechen.
- Bekannt, unabhängig vom Design: xshmfence-Mappings werden beim Restore
  anonym neu angelegt; im Spawn-Pfad fehlt die Hang-Erkennung.
- Kopiermodus für WSL: Bookmark `copy-mode` in gfxstream (6 Commits; die
  Bereichsprüfung ist inzwischen upstream), mesa (4 Commits) und valo
  (`gpu_host_visible_flush_invalidate`). Ungetestet.
- Aus dem Fork nicht übernommen, weil HK sie nicht braucht und kein roter
  Test sie belegt: Catch-all-Barrieren mit `MEMORY_READ|MEMORY_WRITE`,
  Snapshot von BC3/BC7-Images, die 1.1-Instanz-Fähigkeiten. Die
  Mip-Begrenzung ist inzwischen upstream.
