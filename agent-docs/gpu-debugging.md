<!-- // TODO(ai-review): generated, not yet audited -->

# GPU-Checkpoint/Restore: Debugging-Wissen

Stand: Oktober 2026. Profil-Zahlen aus Hollow-Knight-Läufen
(`CP=240 RS=300`); Validation- und Fault-Forensik aus der Aufklärung des
Multi-Slot-DEVICE_LOST (`gpu-stale-descriptor-bindings.md`).

## Architektur des GPU-Modus (Kurzreferenz)

valo orchestratet einen separaten Host-Prozess `kumquat` (gfxstream-basierter
virtio-gpu-Renderer; Forks unter `~/dev/contrib/gpu/{rutabaga_gfx,gfxstream,mesa}`,
Gesamtstand in `~/dev/contrib/gpu/gfxstream/NEXT_STEP.md`):

- Start: `valo-launch` `GpuSession` spawnt kumquat mit
  `--snapshot-base=$VALO_HOME/slots`, legt `$VALO_HOME/gpu-<n>/` an
  (`gpu.sock`, `kumquat.log`) und vererbt dem Game eine Result-Pipe
  (`VALO_GPU_RESULT_FD`). GPU-Snapshots liegen in `slots/<slot>/gpu/`.
- Anfragen: SIGUSR1 (Snapshot) / SIGUSR2 (Restore) per `sigqueue` an
  `KUMQUAT_PID`, der Slot steckt in `si_value`. kumquat blockt beide
  Signale beim Start und liest sie per **signalfd** (`ssi_int` = Slot)
  direkt in seinem epoll-Loop — Anfragen wecken den Loop sofort.
- Host-Visible-Speicher (`BLOB_MEM_GUEST`): kumquat legt pro Blob einen
  memfd an und reicht ihn als udmabuf an gfxstream (Vulkan-Import) und als
  memfd an den Guest-Client, der ihn bis `resource_unref` offen hält. Guest
  und GPU teilen so dieselben Seiten — ein Restore muss genau diesen memfd
  wieder mappen, keine Kopie.
- Antwort: ein Byte auf der Result-Pipe — `S`/`s` (Snapshot ok/fehler),
  `R`/`r` (Restore ok/fehler). valo pollt mit 6 s Timeout (`GPU_WAIT_TIMEOUT`).
- Lifetime: kumquat stirbt per `PR_SET_PDEATHSIG` mit seinem Parent; die
  Result-Byte-Logik bleibt über FD-Restore hinweg stabil, weil valo vor dem
  FD-Restore über ein checkpoint-eigenes Duplikat liest.

Historische Falle, falls jemand wieder auf Flag-Polling umbaut: Signale, die
nur ein Flag setzen, werden erst beim nächsten Loop-Return gesehen, und
magma-gpus `WaitContext` lädt bei EINTR das **volle** Timeout neu —
epoll_wait kehrt trotz SA_RESTART bei Signal-Delivery an Userspace zurück
(per C-Test verifiziert). Das war die Ursache für 500 ms Totzeit pro
GPU-Operation.

## Profilieren

`VALO_USE_GPU=1 just profile` liefert in `out/`:

| Datei                                         | Inhalt                                           |
| --------------------------------------------- | ------------------------------------------------ |
| `perf-{checkpoint,restore}.data` + `.jslb.gz` | perf auf dem Game, Fenster um Checkpoint/Restore |
| `perf-kumquat-{checkpoint,restore}.data`      | perf auf kumquat, gleiche Fenster                |
| `gpu-restore.pftrace`                         | Perfetto track_event um den Restore              |
| `perf-struct.jsonl`                           | strukturierte Events (Phasen-Grenzen)            |
| `perf-timings.txt` / `perf-history.tsv`       | Dauern / Verlauf über Läufe                      |

Dazu: `just profile-summary` (Phasen + Perfetto-Envelopes als Text),
`just profile-report`, `just profile-samply`, `just profile-pftrace`.

## Zeitfenster über Komponenten zusammensetzen

valo (`perf-struct.jsonl`, `ts_us`), perf (`--clockid mono`) und Perfetto
laufen auf derselben CLOCK_MONOTONIC — Zeitstempel direkt in Sekunden
vergleichen. Phasen aus dem Struct-Log:

```
checkpoint: begin → freeze → fds → gpu.trigger → memory → gpu.wait → end
restore:    begin → freeze → apply → memory → gpu → apply.end → end
```

Damit lässt sich jede Phase den perf-Daten zuordnen (z. B. Samples im
kumquat-Perf zwischen `restore.gpu.begin` und `restore.gpu.end` zählen).

## Interpretations-Heuristiken

- **Wenige Samples in einem langen Fenster = idle, nicht Datenverlust.**
  perf sampelt keine wartende CPU. Symptom einer echten Wartephase:
  Samplezahl ≪ Fensterlänge × Frequenz (z. B. 250 Samples in 785 ms @ 999 Hz).
  Für „was passiert während X?" muss die Wall Time über Struct-Events/Perfetto
  kommen, nicht über Sample-Häufung.
- **Perf-Command-Name verrät die Binary-Herkunft**: `.kumquat-wrappe` = das
  nix-gewrappte Store-Binary aus dem valo-devShell, `kumquat` = lokaler
  cargo-Build. Damit erkennt man still-gebliebene alte Binaries in Profilen.
- **kumquat-Checkpoint ≈ `saveImageContent`**: ~90 % der Samples im
  `shmem_file_write_iter`-Pfad (Snapshot liegt in tmpfs → Seitenkopie, kein
  Disk-I/O). Restore spiegelbildlich: `loadImageContent`-Reads plus
  GEM-Neuallokation (`xe_gem_create_ioctl`, `__ttm_pool_alloc`) und Upload.
  Die Dauer ist proportional zur Menge der lebenden VkImages, nicht fix —
  Streuung zwischen Läufen ist Szenen-Varianz.

## Fallen

- **Welcher kumquat, welches gfxstream?** Auf dem Desktop ist der
  PATH-kumquat ein `cargo install` aus rutabaga_gfx und lädt
  `libgfxstream_backend.so` direkt aus `~/dev/contrib/gpu/gfxstream/build`
  (`ldd $(which kumquat)`) — gfxstream-Änderungen brauchen nur
  `ninja -C build`, kumquat-Änderungen ein neues `cargo install`. Ein
  laufender kumquat behält die alte Lib: Läufe, die vor dem Build
  gestartet wurden, zählen nicht. Mit dem nix-devShell kommt kumquat
  stattdessen aus dem valo-Flake (`nix flake update kumquat`, neue Shell).
- **Schnelle Iteration ohne nix-Rebuild**: im rutabaga_gfx-devShell
  `cargo build --release -p kumquat_virtio --features gfxstream` (Ziel-Dir
  ist wegen globaler cargo-config `~/.cache/rust`), dann per
  `VALO_KUMQUAT_BIN` einen Wrapper einhängen, der
  `LD_LIBRARY_PATH=<gfxstream-backend>/lib` setzt (das Store-Binary bekommt
  das via wrapProgram, ein cargo-Build nicht).
- **Wrapper nicht in /tmp ablegen**: steam-run mountet ein privates /tmp in
  die FHS-Sandbox — der Wrapper wäre im Game-Prozess unsichtbar (ENOENT).
  Unter `$HOME` funktioniert es.
- **kumquat-Leichen** (nur relevant, falls das Binary vor dem PDEATHSIG-Fix
  lief): bei hartem Parent-Kill (SIGINT/Timeout) läuft der Drop nie, kumquat
  wird an `systemd --user` vererbt und blockt ewig. Erkennen via
  `pgrep -af kumquat`.
- **`pkill -f <muster>` killt die eigene Shell**, wenn das Muster in der
  eigenen Kommandozeile steht: `pkill -x kumquat` bzw. Bracket-Trick
  (`[k]umquat`).
- **Gemeinsamer `CARGO_TARGET_DIR` über jj-Workspaces/Klone** vergiftet
  Builds gegenseitig: ein Build im anderen Workspace überschreibt das
  Binary, der erste hält es per mtime für frisch (Symptom z. B. „postcard
  decode: Found a bool that wasn't 0 or 1"). Reparatur: Quellen `touch`en.

## Validation Layers (VVL) im Host

Der Layer muss in kumquat laufen (dort spricht gfxstream mit dem echten
Treiber), nicht im Game-Prozess auf dem gfxstream-ICD. Also über einen
`VALO_KUMQUAT_BIN`-Wrapper setzen, nicht global:

```bash
export ANDROID_EMU_VVL_BEHAVIOR=print     # gfxstream aktiviert Layer + Messenger
export GFXSTREAM_TESTDATA_PATH=/usr/share/vulkan/explicit_layer.d
export VK_KHRONOS_VALIDATION_REPORT_FLAGS=error,warn
export VK_KHRONOS_VALIDATION_DUPLICATE_MESSAGE_LIMIT=50
export VK_KHRONOS_VALIDATION_GPUAV_ENABLE=1   # optional, siehe unten
exec kumquat "$@"
```

- Meldungen landen über gfxstreams Debug-Messenger im kumquat-Log
  (`VVL [App: …, Engine: …]: …`), nicht in einer eigenen Datei.
- gfxstream sucht `VkLayer_khronos_validation.json` nur neben dem Programm
  oder unter `GFXSTREAM_TESTDATA_PATH`; ohne das: „Vulkan validation layer
  library path not found … Skipping“. Ob der Layer wirklich geladen ist,
  zeigt die Loader-Zeile `Insert instance layer "VK_LAYER_KHRONOS_validation"`;
  `Invalid JSON file …` heißt kaputte Installation (0-Byte-Dateien nach
  Reboot direkt nach `pacman -S` → `pacman -S --overwrite '*' … && sync`).
- Grundrauschen ohne Restore (nicht der Bug): `synchronization2` nicht
  aktiviert, `VkBufferMemoryRequirementsInfo2` mit falschem `sType`,
  Barrier-/Layout-Meldungen, Import-Handle-Type-Mismatch bei
  `vkBindBufferMemory`. Relevant ist, was erst nach einem Restore auftaucht:
  per Zeilenbereich zwischen `restore (slot N) done` und dem Fault filtern.
- **GPU-AV** (`VK_KHRONOS_VALIDATION_GPUAV_ENABLE=1`, nicht
  `…_VALIDATE_GPU_BASED`) instrumentiert Shader und findet, was Core
  Validation nicht sehen kann: Vertex-Fetch-OOB
  (`VUID-vkCmdDrawIndexed-None-02721`, inkl. Buffer, Offsets, Index-Buffer),
  Descriptor- und Buffer-OOB. Langsam, und fängt den Zugriff ab — ein
  GPU-AV-Lauf faultet meist nicht, liefert aber den verursachenden Draw.
- Env-Werte sind strikt: ein kaputtes Wrapper-Skript (`=1export …`) meldet
  sich nur als `LAYER SETTING (gpuav_enable) error … not a boolean`.

## GPUVM-Fault-Forensik

- **Ein Fault resetet die ganze GPU** (`amdgpu … GPU reset … device
  wedged`): alle GPU-Clients verlieren ihren Kontext, Compositor/Browser
  sterben, oft bis zum Reboot. Logs deshalb nicht nur in `/tmp` (tmpfs)
  halten: im Wrapper `exec kumquat "$@" > >(tee -a ~/.cache/kumquat-live.log) 2>&1`
  plus Hintergrund-`sync` im Sekundentakt. `exec` ist Pflicht — valo
  signalisiert kumquats PID.
- **Adresse → BO**: `RADV_DEBUG=bo_history` loggt jedes BO-Map/Unmap mit
  VA-Bereich und `os_time_get_nano`-Zeitstempel nach
  `/tmp/radv_bo_history.log` (fester Pfad, pro Winsys mit `w+` geöffnet →
  enthält nur das letzte Device; zum Überleben eines Reboots als Symlink
  nach `$HOME` anlegen). Liegt die Fault-VA genau hinter einem BO-Ende, ist
  es ein OOB-Read, kein Use-after-free.
- **BO → Vulkan-Objekt**: CLOCK_MONOTONIC-Zeitstempel um
  `vkAllocateMemory` loggen und gegen die BO-History matchen.
- **Gleiche Fault-VA über Läufe** heißt deterministisches VA-Layout, nicht
  zufälliger Müll. Ob ein OOB-Read faultet oder still Nachbar-Speicher
  liest, hängt am Layout — „mal grün, mal Fault“ ist dann keine echte
  Varianz der Ursache.
- **Guest-Mappings prüfen**: PID des Spiels ist die erste `[pid=…]` im
  `valo.log` (nicht `valo-<pid>`, das ist der Launcher). Host-Visible-Blobs
  erscheinen als `/memfd:kumquat-guest-blob (deleted)`; ein
  `rw-s … /dev/zero (deleted)` an derselben Adresse nach einem Restore
  heißt: valo hat das Mapping anonym neu angelegt, Guest und GPU sehen
  verschiedene Seiten.
- Vor jedem Lauf `just clean` (celestetas-rs): ein Multi-Slot-Lauf belegt
  ~4 GB in `/tmp`; volles tmpfs bricht Läufe am Checkpoint ab.

## Debug-Artefakte und Logs

- Run-Home: `$VALO_HOME` (Default `/tmp/valo-<pid>`, `/tmp/valo-latest`
  zeigt auf den letzten Lauf): `valo.log`, `valo-struct.jsonl`,
  `trace.log`, `slots/`.
- kumquat-Log (Renderer-Features, „snapshot/restore (slot N) done",
  `[reattach]`, `[detect]`): `$VALO_HOME/gpu-<n>/kumquat.log` —
  stdout/stderr des Hosts.
- valo-Events zu fds: `checkpoint.fds(.pin)`, `restore.fds(.reopen/.close)`
  in `valo-struct.jsonl` (welche memfd/dmabuf-Inode wann gepinnt wurde).
- Vulkan-Commands im kumquat-Log: `ANDROID_EMU_VK_LOG_CALLS=1`.
