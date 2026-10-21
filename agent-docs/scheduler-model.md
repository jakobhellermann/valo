<!-- // TODO(ai-review): generated, not yet audited -->

# Virtueller Scheduler: der unendlich schnelle Rechner

Stand: Oktober 2026. Idee, kein Code. Anlass war der Boot-Hänger von HK 1.2.2.1
unter virtuellen Main-Thread-Sleeps; das Modell zielt aber auf alle Spiele.

## Prioritäten (kontrollierter Modus)

1. **100 % deterministisch in den beobachtbaren Effekten** (Spielzustand pro
   Frame, Bild, RNG). Welche Instruktionen wann laufen und wie oft eine
   Schleife ohne Effekt dreht, ist egal.
2. **Sauberes Modell**: eine realistische Ausführung des Spiels auf einem
   schnellen Rechner, keine Spiel-Hacks als Fundament.
3. **Performance** nah an nativ.

## Anlass: HK 1.2.2.1 bleibt schwarz

Unity 5.4 (gestrippte Exe) wartet beim Boot in
`PreloadManager::WaitForAllAsyncOperationsToComplete` (exe `0x84f4b8`):

```
do {
    UpdatePreloadingSingleStep(pm, flags=2, INT_MAX);   // 0x84ebc0
    Thread::Sleep(0.01);                                // nanosleep 10 ms
    lock(pm+0x90); pending = pm[0xd0] || pm[0xf0]; unlock;
} while (pending);
```

Der virtuelle Sleep kehrt sofort zurück und schiebt die globale Uhr um 10 ms
vor. Die Schleife dreht in Mikrosekunden, die virtuelle Zeit rast; der
Loader-Thread rechnet in echter Geschwindigkeit, misst aber an derselben Uhr.

| Variante | Ergebnis |
| --- | --- |
| Main-Sleep virtuell, Uhr läuft weiter | schwarz bis ~30 s |
| Main-Sleep virtuell, Uhr läuft **nicht** weiter | Menü |
| echter Sleep | Menü, 6 Main-Sleeps |
| Drain (s. u.) | Menü, aber 170 **oder** 70 Main-Sleeps |

Ausgeschlossen: FMODs 100-Hz-`usleep` (Audio-Mixer), Unitys Job-Worker
(`Sleep(0)`/`sem_wait`), Mutex-Starvation, Deadlines in der Vergangenheit,
GPU-Host (kumquat in die Idle-Prüfung genommen: unverändert).

libTAS: für Unity-Spiele erzwingt `transfer_sleep` „Sleep stellt die Uhr nie
vor“ (Commit 85cb688d), Determinismus des Ladens kommt dort aus Unity-Hooks
(`UnityHacks.cpp`, `PreloadManager`/`JobQueue`).

## Drain-Experiment

Vor jedem virtuellen Main-Sleep echt warten, bis alle anderen Threads idle
sind (`/proc/self/task/*/stat` in `S`, oder `sched_yield`/`Sleep(0)` in der
letzten 1 ms), dann die Uhr vorstellen. Ergebnis:

- lädt zuverlässig, ist aber **nicht deterministisch** (170/170/170/170/70/70).
  Grund: zwischen zwei Sleeps läuft der Main-Thread parallel zum Loader;
  wie weit der ist, wenn der Main-Thread nachsieht, ist echtes Timing.
- nach ~16 Sleeps ist bei jedem Drain schon alles idle, die Schleife dreht
  trotzdem weiter — worauf, ist offen.
- ein `/proc`-Scan kostet ~0,9 ms: als Mechanismus zu teuer.

## Modell

Das Spiel sieht nur Ergebnisse, nie Zwischenstände: auf einem unendlich
schnellen Rechner ist angestoßene Arbeit fertig, bevor der Anstoßende
weiterrechnet.

1. **Wake des Frame-Threads** (`FUTEX_WAKE`, `sem_post`, `cond_signal`,
   Unlock mit Wartendem, `pthread_create`, Write auf Pipe/eventfd): danach
   wartet er, bis alle anderen idle sind.
2. **Zeit bewegt sich nur, wenn alle anderen idle sind**, und springt auf die
   früheste Deadline. Sleeps und Timeouts *aller* Threads sind virtuell
   (FMODs 10 ms sind 10 virtuelle ms); zwischen zwei Timer-Events wieder
   bis idle warten.
3. **Die anderen laufen untereinander parallel.** Ihre Reihenfolge ist egal,
   solange das Ergebnis nicht davon abhängt (Job-Systeme realistischer
   Spiele).
4. **`yield`/`Sleep(0)` zählt als idle**: Warten auf jemand anderen. Reicht
   nicht für Lock-free-Übergaben an Spinner, siehe „Offene Designfrage“.
5. **GPU und Audio sind unendlich schnelle Geräte**: wer auf den GPU-Host
   wartet, ist busy; Fence-Abfragen sehen „fertig“; Audio konsumiert in
   virtueller Zeit.

Realistisch, weil es eine gültige Ausführung auf einem (sehr) schnellen
Rechner ist. Ein Einkern-Scheduler mit Branch-Counter-Preemption (rr) wäre
nur für Determinismus auf Instruktionsebene nötig.

## Mechanismus: seccomp

LD_PRELOAD-Hooks sehen nicht jedes Blockieren: rohe `syscall(SYS_futex)`
(Unity 6000 in `AddToQueue`, siehe `unity-preload.md`), glibc-interne Locks
(malloc, stdio, ld.so), libstdc++ `atomic::wait`, Rust-std, statisch gelinkte
Bibliotheken. Das Modell braucht aber *jedes* Blockieren und *jeden* Wake.

Ein seccomp-Filter meldet genau `futex`, `nanosleep`/`clock_nanosleep`,
`poll`/`ppoll`/`select`/`pselect`/`epoll_*`, `sched_yield` und die
Wake-Wege per `SECCOMP_RET_TRAP` (SIGSYS); valos eigene Syscalls sind über den
Instruction Pointer ausgenommen. Der Handler emuliert den Syscall aus dem
`ucontext` und kennt damit pro Thread seinen Zustand — „alle idle“ wird ein
Zähler plus Futex statt Polling.

- **Die Semantik steckt im Syscall**: `FUTEX_WAIT` relativ auf MONOTONIC,
  `FUTEX_WAIT_BITSET` absolut mit `FUTEX_CLOCK_REALTIME`-Flag,
  `clock_nanosleep` mit Clock und `TIMER_ABSTIME`. Die Condvar-Clock-Tabelle
  (`determinism/deadline.rs`, `COND_CLOCKS`) wird überflüssig.
- **Gleiche Zeit-Logik, eine Stelle**: `determinism/deadline.rs` und
  `timeout.rs` sind die Bausteine, die der Handler aufruft; die Symbol-Hooks
  für Waits und Sleeps entfallen (oder bleiben als Fast-Path gegen
  Trap-Kosten, Korrektheit kommt von seccomp).
- **Bleibt Hook**: Uhr lesen (`clock_gettime` usw. laufen über den vDSO, kein
  Syscall — die Futex-Deadline ist nur virtuell, wenn der Aufrufer virtuell
  gelesen hat), GL/Vulkan/X, Input, Present, Unity-Hooks.
- **Kommt dazu**: Kernel-Timer (`timer_settime`, `setitimer`, `alarm`,
  `timerfd_settime`) laufen heute in Echtzeit und müssen virtuelle Timer
  werden.
- **Prozessübergreifende Futexe** (ohne `FUTEX_PRIVATE_FLAG`, geteiltes
  Mapping, evtl. gfxstream-Ring): Warten darauf ist „wartet auf Gerät“,
  also busy.

### Voraussetzungen

- Keine Capabilities: `prctl(PR_SET_NO_NEW_PRIVS, 1)` vor dem Filter genügt;
  `CONFIG_SECCOMP_FILTER` ist Distro-Standard.
- `NO_NEW_PRIVS` und der Filter sind unumkehrbar und vererben sich über
  fork/exec: setuid-Helfer des Spiels verlieren Rechte, und ein Kindprozess
  ohne SIGSYS-Handler stirbt am ersten getrappten Syscall (z. B. `futex` in
  ld.so vor libvalos Konstruktor). Prüfen, ob Zielspiele Kinder starten;
  sonst `SECCOMP_RET_USER_NOTIF` an einen valo-Supervisor.
- Alternative Syscall User Dispatch (`PR_SET_SYSCALL_USER_DISPATCH`, ab 5.11):
  ohne `NO_NEW_PRIVS`, bei exec gelöscht, aber nicht selektiv — jeder Syscall
  außerhalb von libvalo trappt (GPU-ioctls!). Vererbung an Threads ungeprüft.
- SIGSYS-Handler, die blockieren, müssen mit valos Park-Signalen für
  Checkpoints zusammenspielen (`docs/signal-state.md`); Scheduler-Zustand
  gehört in den Snapshot.

## Verhältnis zu `gamespecific::unity`

Die Unity-6-Hooks setzen dasselbe Modell schon spielspezifisch um:
`preload.rs` (Preload-Thread idle vor `UpdatePreloading`), `TryTimedAcquire`
in `WaitForAllAsyncOperationsToComplete` (erst `wait_until_idle`), `jobs.rs`
(Job des Main-Threads fertig, bevor er weiterrechnet), synchrone Async-Reads.
Sie hängen an aufgelösten Symbolen und der Build-ID: HK 1.2.2.1 (Unity 5.4,
gestrippt) und andere Engines deckt das nicht ab, ungehookte Subsysteme
(FMOD, C#-Threads/Tasks, Render-Thread, GPU-Uploads, Steam) auch nicht.

Mit dem Scheduler entfallen sie größtenteils:

- **Preload**: `AddToQueue` weckt per rohem Futex → Regel 1 → `Perform` läuft
  durch; eine nicht-parallele Operation, die auf den Main-Thread wartet,
  blockiert am Futex und ist idle.
- **`TryTimedAcquire`**: alle idle → Zeit springt 1 ms → Erfolg oder
  deterministischer Timeout.
- **Async-Reads**: ein IO-Thread im Datei-`read` ist busy, kein Wait.
- **Jobs**: nur mit der Regel aus der offenen Designfrage.

Danach blieben sie Optimierung (warten gezielt auf relevante Arbeit statt auf
alle Threads) und Diagnose (welche Operation in welchem Frame fertig wird).

## Offene Designfrage: Lock-free-Übergabe an Spinner

Unity übergibt Jobs lock-free; einen Futex-Wake gibt es nur, wenn ein Worker
wirklich schläft. Drehen die Worker ihre `Sleep(0)`-Schleife, schiebt der
Main-Thread den Job ohne sichtbaren Syscall in die Queue und rechnet weiter,
während ein Spinner ihn parallel aufnimmt — das Rennen, das `jobs.rs` heute
semantisch abfängt. Mit „`yield` zählt als idle“ sieht der Scheduler es nicht.

Kandidat: **`yield` blockiert, solange der Main-Thread läuft.** Spinner kommen
erst dran, wenn der Main-Thread blockiert (typisch: er wartet auf den Job).
Deterministisch und realistisch; offen ist ein Main-Thread, der selbst per
`yield` auf Jobs wartet (Turn-Taking statt Deadlock). Bis der Prototyp das
klärt, bleibt `jobs.rs` nötig.

## Performance

- Verlust nur die Überlappung von Frame-Thread und Hintergrundarbeit; Unity
  wartet auf Jobs meist ohnehin direkt nach dem Anstoßen. Laden wird
  spürbar langsamer, dauert dafür immer gleich viele Frames.
- Trap grob 2–4 µs; bei 10–30k Futex-Aufrufen/s etwa 5–10 % eines Kerns
  (Schätzung, zu messen).
- Nie Warten auf echte Zeit: Fast-Forward ist nur durch Rechenarbeit begrenzt.

## Restrisiken

- Reihenfolge-abhängige Effekte zwischen Workern (Heap-Adressen nach
  Laufreihenfolge → adressbasierte Hashes → Iterationsreihenfolge).
  Per Orakel prüfen, nicht vorab lösen.
- Busy-Loops ganz ohne Syscall werden nie idle: lauter Timeout.
- Direkte vDSO-Reads an den Hooks vorbei sehen echte Zeit (heute schon so).

## Prototyp-Plan

Orakel sind beobachtbare Effekte, nicht Sleep-Zahlen: Frame-Inhalt/RNG-State
pro Frame über 10 Läufe von HK 1.2.2.1 bis zum Menü, und für Unity 6000 die
Messung aus `unity-preload.md` (`IntegrateMainThread` heute Frame 3 vs. 42).

1. seccomp-Tracking + „nach Wake des Frame-Threads bis idle warten“, Zeit wie
   bisher. Verschwindet 170/70? Trap-Kosten messen.
2. Timer aller Threads virtuell, inkl. Kernel-Timer.
3. GPU-/Audio-Geräte, wo das Orakel etwas findet.

## Offen

- Worauf die HK-1.2.2.1-Schleife nach dem Ladeschub noch ~50–150 Durchläufe
  wartet (Kandidat: `IsDone` der Operation in `pm+0xe0`).
- Was valo mit der Audio-Ausgabe macht.
- Ob gfxstream/kumquat geteilte Futexe nutzt.
