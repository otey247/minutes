# Windows live transcription: capture, inference, and relay repair

The reported failure had an intact recording WAV but sparse finalized live
text, missing current-speech drafts, and inconsistent relay cursors. The repair
keeps the existing Whisper recording sidecar and application-audio capture.
It does not change the global model, recognition backend, or persisted transcript
format.

## Causes and changes

Native audio callbacks can contain approximately 10 ms of audio. The energy VAD
assumes 100 ms calls, while the Whisper Silero adapter rescans a rolling window.
Passing callbacks through directly both shortened the energy gate's hangover and
caused excessive Silero work. The consumer now assembles exact 1,600-sample frames
at 16 kHz, with source offsets and capture timestamps. Silero evaluates every
200 ms, retaining all intervening audio and 500 ms of onset pre-roll. Missing
source samples reset the utterance and draft state rather than silently joining
audio across a dropped packet. The handoff reserves a fixed budget of audio
samples before publishing a packet and releases that reservation on consumption
or disconnect. Its 200-frame capacity therefore means 20 seconds, independent of
callback size. Capture still uses a bounded, nonblocking handoff; the optional
consumer cannot block the WAV writer.

The sidecar also fed already segmented utterances through progressive Whisper
recognition, throwing away intermediate results. It now makes one recognition
call per snapshot and keeps the model loaded. Speech-confirmed replies shorter
than one second are silence-padded for inference while retaining their actual
duration. Final jobs have priority. Replaced utterances cancel in-flight drafts,
and old snapshots expire using their original capture time. Queue accounting
reserves its pending count before publishing work, eliminating a fast-consumer
underflow race.

On Windows, `PeekNamedPipe` could report an empty pipe while `BufReader` still
held replay data. The client now drains buffered bytes first, reads only bytes
available from the pipe, and retains incomplete UTF-8/JSON frames across calls.
Session-specific pipe names prevent independent recorders/test homes from
colliding. A consumed session-reset handshake also resets the client's cursor.
The CLI stops replay at its initial heartbeat watermark, checks its 350 ms
deadline on every iteration, and withholds a draft if replay is incomplete.

Status now includes model readiness, backend, whether GPU support was compiled,
audio gaps, final queue pressure, recognition attempts/empty results/errors/
cancellations, filtered output, and successfully published finals. GPU compilation
is not proof of runtime offload. Final age comes from the last successful final
publication; a heartbeat no longer makes stale text look fresh. Drafts remain
ephemeral and do not enter the durable JSONL transcript.

## Windows verification

The CLI and desktop application build successfully with `vulkan,pocketstation-capture`.
The replay harness uses isolated output and the production recording-sidecar path.
It accepts arbitrary callback sizes, repeat counts, reference text, and an optional
relay. It reports draft freshness, final latency, word error rate, audio drops,
and the existing worker timing summary.

| Run | First draft p95 | Source end to final p95 | Audio/final queue drops |
| --- | ---: | ---: | ---: |
| Vulkan base, 35 s, 10 ms callbacks | 1,292 ms | 1,093 ms | 0 / 0 |
| Vulkan small, 35 s, irregular callbacks | 1,709 ms | 706 ms | 0 / 0 |
| CPU base, 19 s, 100 ms callbacks | 2,000 ms | 1,575 ms | 0 / 0 |
| Final Vulkan base, 907 s, 10 ms callbacks | 1,476 ms | 916 ms | 0 / 0 |
| Final Vulkan base.en, 35 s, 10 ms callbacks | 1,472 ms | 980 ms | 0 / 0 |

The final 15-minute run delivered all 90,682 input packets, 234 final lines,
and 355 fresh draft events. Maximum delivered draft age was 890 ms; maximum
source-end-to-final latency was 1,283 ms. Recognition failures and final-queue
drops were zero. Strict WER remained 28.06%, so passing delivery/latency gates
does not establish the separate accuracy gate.

The initial 907-second soak exposed 128 dropped 10 ms packets during transient
consumer stalls, despite fast recognition. The old 200-packet queue represented
only two seconds on this device. This finding led to the sample-budget queue
above; the regression test fills exactly the same audio duration with 10 ms,
100 ms, and 200 ms packets and verifies capacity recovery after disconnect.
An overload run concurrent with release compilation also exhausted the new
budget and was stopped. Sustained CPU starvation can still produce explicit
live-audio gaps; the WAV writer remains isolated. Normal-load acceptance was
restarted after compilation finished.

The first two runs replay `apple-speech-meeting.wav` three times with a one-second
silence between repeats. The CPU run uses `apple-speech-dictation.wav`. These are
synthetic fixtures, not a substitute for a labeled recording of a real call.
Final latency includes consumer/VAD delay, so it is stricter than measuring only
from final-job enqueue. Short runs do not establish a general latency guarantee.

Strict normalized WER on the names-heavy meeting reference was 29.49% for live
base and 21.79% for live small. The full-file base baseline scored 19.23%; it misspells Parakeet
and renders SpeechTranscriber/DictationTranscriber as separate words. The 15% WER
acceptance target is **not established** by these results. No fixture-specific
rewriting, proper-name substitution, or relaxed scoring was added to claim a pass.
Model/context tuning and representative human-labeled audio remain necessary.

An additional CPU replay with `base.en` reduced strict WER to 17.95%, with no
audio/final queue drops, but first-draft p95 was 2,392 ms. This profile still does
not establish acceptance. The final GPU base.en run retained 17.95% WER with
1,472 ms first-draft p95 and 980 ms final p95. Its same-model full-file baseline
scored 19.23%. Thus the relative accuracy and latency gates pass for this short
fixture, while the absolute 15% WER gate remains open. Its weights came from the
[whisper.cpp model source](https://github.com/ggml-org/whisper.cpp/tree/master/models)
and matched the published SHA-1 `137c40403d78fd54d454da0f9bd998f78703390c`.
The model and its configuration remain isolated from the active user profile.

Automated checks completed:

- Final core no-default-features suite with isolated HOME: 1,721 passed,
  zero failed, five ignored. An earlier non-isolated run exposed one
  Windows permission-sensitive test; it passes in the isolated full run.
- Live-transcript suite: 50 passed, two ignored.
- Final capture suite: 59 passed, four ignored.
- Relay suite: 17 passed, including buffered bursts, fragmented frames, and
  session cursor reset.
- Queue isolation/accounting: two passed; streaming Whisper: eight passed;
  sidecar-focused suite: 61 passed; final audio framing/budget suite: three passed.
- Focused core/CLI Clippy with Whisper and Rust formatting passed.
- MCP TypeScript/UI build and the capture-attachment unit tests passed.
- An actual stdio MCP server, pointed at an isolated replay and patched CLI,
  returned 18 successful reads: eleven fresh draft snapshots, four accumulated
  finals, monotonic relay cursors, and model readiness. The slowest read took
  431 ms end to end. Its temporary `.minutes` directory used the existing owner-only
  Windows DACL contract; the policy check was not bypassed.

Whole-workspace Clippy is blocked by the pre-existing Windows-only dead-code
warning for `WORKER_CPU_SECONDS` in `crates/archive-semantic/src/lib.rs`. The
earlier permission-sensitive core failure used the actual HOME instead of a test home;
no real meeting-directory permissions were changed to make the suite pass.

The replay intentionally disconnects its feeder at EOF so the worker drains all
queued finals. The production boundary logs this as an unexpected recording
feeder disconnect. This expected harness diagnostic is not a recognition failure;
use `whisper_failures` and drop counters to assess the replay.

## Reproduction and remaining acceptance

With the repository's pinned Rust toolchain, LLVM, and Vulkan SDK available:

```powershell
cargo build --release -p minutes-cli --features vulkan,pocketstation-capture
cargo build --release -p minutes-app --features vulkan,pocketstation-capture
cargo build --release -p minutes-core --example replay_sidecar --features streaming,vulkan
```

Use a process-scoped `MINUTES_CONFIG_PATH` pointing to a test configuration with
an absolute model directory, English, `partial_max_secs = 8`, live model `base`,
and `max_utterance_secs = 5`. This avoids modifying the user's active profile.
Extract `referenceText` for `meeting-longer` from
`tests/eval/fixtures/apple-speech-corpus.json` into a text file, then run:

```powershell
$env:RUST_LOG = 'warn'
& "$env:CARGO_TARGET_DIR/release/examples/replay_sidecar.exe" `
  --wav tests/eval/fixtures/audio/apple-speech-meeting.wav `
  --reference meeting-reference.txt --realtime --drafts `
  --chunk-samples 160 --repeat 78
```

If `CARGO_TARGET_DIR` is unset, use `target/release/examples/replay_sidecar.exe`.
The repeat count produces just over 15 minutes. Re-run short tests with callback
sizes `1600` and `71,800,3199,160`, and omit Vulkan when testing CPU fallback.

The existing real recording was left running during the September 30 repair.
On October 1, no recording was active, so the patched CLI was installed in
`~/.minutes/bin` with its Visual C++ runtime DLLs, and the verified `base.en`
model was installed in `~/.minutes/models`. The previous config was backed up;
`[live_transcript] model` changed from `tiny` to `base.en`, and its utterance cap
was set to the replay-tested five seconds. A current-user
NSIS desktop installer completed successfully, and the installed app launched.
A fresh `minutes-mcp` 0.27.0 stdio session returned a successful
`read_live_transcript` result against the installed CLI while idle. The EMEET
SmartCam C960 4K microphone appears in `minutes devices`.

Real EMEET capture and remote-participant validation in Teams/Zoom are still
open. Hardware acceptance must include short acknowledgments, continuous
speech, silence, quiet speech, and stop/WAV preservation. Compare those finals
against both a human reference and the same-model full-file baseline. Follow-up
work is tracked in the local Beads store; this document records evidence, not
task state.
