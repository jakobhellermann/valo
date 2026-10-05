<!-- // TODO(ai-review): generated, not yet audited -->

# GPU-Restore DEVICE_LOST nach Cross-Slot-Restore (gelöst)

Stand: 11. Oktober 2026. Fallbericht; Werkzeuge in
`agent-docs/gpu-debugging.md` („Validation Layers", „GPUVM-Fault-Forensik").

## Symptom

`just run-tas tases/multislot_gpu.tas --use-gpu` (save 0, save 1, load 0,
load 1): nach `restore slot 1` Glitches, dann
`radv: GPUVM fault detected at address 0x80012d900000` →
`VK_ERROR_DEVICE_LOST` → SIGABRT in kumquat; der amdgpu-Reset nimmt oft den
Desktop mit. ~4 von 5 Läufen, immer dieselbe VA.

## Ursache

valo legte beim Restore ein Mapping, das zwischen den beiden Snapshots
entstanden war (Unitys 4-MiB-Host-Visible-Ring, `/memfd:kumquat-guest-blob`),
als **anonymes** Shared-Mapping neu an: `reallocate_area` öffnete die Datei
nur für `MMapPath::Path`, `/memfd:` ist `MMapPath::MemFd`. Die GPU las weiter
den memfd (udmabuf-Import), der Guest schrieb Vertex-/Index-Daten ins
anonyme Mapping → veraltete Indizes (65535 …) → Vertex-Fetch hinter das
Buffer-Ende → Fault, sobald dahinter die 2-MiB-Alignment-Lücke lag.

Messkette: BO-History (Fault-VA = 1 Byte hinter einem 4-MiB-BO) →
`vkAllocateMemory`-Zeitstempel (BO = Ring) → GPU-AV
`VUID-vkCmdDrawIndexed-None-02721` (nur nach `restore slot 1`) → Host
importiert korrekt den Guest-memfd (Inode stabil) → `/proc/<pid>/maps`:
Ring nach dem Restore `/dev/zero (deleted)` statt memfd.

## Fix

valo `checkpoint: re-create missing memfd and shm mappings from their file`
(+ Test `mmap_memfd_shared_realloc`): Reopen auch für `MemFd`/`Shm`,
writable Shared-Mappings mit `O_RDWR`, `error!` falls ein Shared-Mapping
doch anonym neu angelegt werden muss. Die Inode findet sich über die
fd-Keepalives des Checkpoints (die stehen in der live fd-Tabelle; der
fd-Sync läuft erst nach dem Speicher-Restore). Validiert: 3/3 TAS-Läufe
ohne Fault.

## Widerlegte Spuren (nicht erneut verfolgen)

- **Stale Descriptor-Writes** (View außerhalb des Replay-Fensters, Load
  skippt den Write): gfxstream-Commit `vk: snapshot load - bind stale
  descriptor writes as null descriptors` ändert am Fault nichts; ob er ein
  beobachtbares Problem behebt, ist offen.
- Uninitialisierter Descriptor-Pool-Speicher (`RADV_DEBUG=zerovram`: kein
  Effekt), zu große Image-/Buffer-Binds (Bind-Range-Check: keine Treffer).

## Offene Nebenbefunde (eigene Fehler, nicht dieser)

- **Zerstörte ImageViews im Snapshot-Load**: VVL meldet in manchen Läufen
  `vkUpdateDescriptorSets … Invalid VkImageView` — unbox gelingt, die
  Host-View existiert nicht mehr.
- VVL nach Restore: `vkBeginCommandBuffer` auf *executable* Command-Buffern
  (Pool ohne RESET-Bit), doppelt signalisierte binäre Semaphoren
  (`VUID-vkQueueSubmit-pSignalSemaphores-00067`).
- Gelegentlicher Post-Restore-Livelock ohne Fault: nach
  `restore (slot 1) done` kein GPU-Traffic mehr, ein Guest-Thread spinnt
  auf `clock_gettime(MONOTONIC)`.
- memfds, deren fd schon vor dem Checkpoint geschlossen war (Kandidat
  `/memfd:xshmfence`, heute `could not open backing file … EPERM`), kann
  valo beim Restore nicht neu mappen — das meldet jetzt der `error!`.
