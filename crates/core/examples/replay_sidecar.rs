//! Replay a 16 kHz mono WAV through the production recording sidecar.
//!
//! This is the harness that found the whisper-rs abort-callback regression
//! (#951) and measured the drop behaviour behind #967. It drives
//! `run_sidecar_mpsc`, the exact code path the desktop recorder uses, from a
//! WAV instead of the microphone, under an isolated HOME that reuses the real
//! models and config, with a tracing subscriber so every warning the desktop
//! app would swallow is printed.
//!
//! ```text
//! cargo run --release -p minutes-core --example replay_sidecar \
//!     --features "whisper streaming" -- --wav call.wav --realtime --drafts
//! ```
//!
//! - `--realtime` paces chunks at 100 ms and uses the production
//!   20-second sample budget + `try_send`, so feed drops are visible. Without it
//!   the audio is pushed as fast as the sidecar accepts it.
//! - `--drafts` wires a live-partial publisher like the desktop recorder does,
//!   so the worker also runs current-speech drafts.
//! - `--speed N` scales the real-time pacing (2 = twice real time).
//! - `--chunk-samples 160,1600,4096` cycles device callback sizes.
//! - `--repeat N` repeats the fixture with one second of silence between runs.
//! - `--reference PATH` scores normalized word error rate against a text file.
//! - `--relay` exposes the isolated capture relay for CLI/MCP smoke tests.
//!
//! A WAV snapshotted mid-recording (unfinalized header) is read as raw
//! `pcm_s16le`. Add `--features metal` to match the shipped macOS build.
#![cfg_attr(
    not(all(feature = "whisper", feature = "streaming")),
    allow(dead_code, unused_imports)
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn word_error_rate(reference: &str, hypothesis: &str) -> f64 {
    let words = |text: &str| {
        text.split_whitespace()
            .map(|word| {
                word.chars()
                    .filter(|c| c.is_alphanumeric())
                    .collect::<String>()
                    .to_lowercase()
            })
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
    };
    let expected = words(reference);
    let actual = words(hypothesis);
    let mut row: Vec<usize> = (0..=expected.len()).collect();
    for (i, word) in actual.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, expected_word) in expected.iter().enumerate() {
            let previous = row[j + 1];
            row[j + 1] = (row[j] + 1)
                .min(previous + 1)
                .min(diagonal + usize::from(word != expected_word));
            diagonal = previous;
        }
    }
    row[expected.len()] as f64 / expected.len().max(1) as f64
}

fn read_wav(path: &Path) -> Vec<f32> {
    let bytes = std::fs::read(path).expect("read wav");
    if bytes.len() > 44 && &bytes[0..4] == b"RIFF" {
        let data_len = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]) as usize;
        if data_len == 0 || data_len > bytes.len() - 44 {
            eprintln!("wav: unfinalized header, reading raw pcm_s16le mono 16 kHz");
            return bytes[44..]
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / i16::MAX as f32)
                .collect();
        }
    }
    let mut reader = hound::WavReader::open(path).expect("open wav");
    let spec = reader.spec();
    assert_eq!(spec.sample_rate, 16_000, "expected a 16 kHz wav");
    let raw: Vec<f32> = reader
        .samples::<i16>()
        .filter_map(|s| s.ok())
        .map(|s| s as f32 / i16::MAX as f32)
        .collect();
    if spec.channels == 1 {
        raw
    } else {
        raw.chunks(spec.channels as usize)
            .map(|f| f.iter().copied().sum::<f32>() / f.len() as f32)
            .collect()
    }
}

#[cfg(all(feature = "whisper", feature = "streaming"))]
fn main() {
    let mut args = std::env::args().skip(1);
    let mut wav: Option<PathBuf> = None;
    let mut drafts = false;
    let mut realtime = false;
    let mut speed: f64 = 1.0;
    let mut chunk_sizes = vec![1600usize];
    let mut expose_relay = false;
    let mut repeat = 1usize;
    let mut reference: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--wav" => wav = args.next().map(PathBuf::from),
            "--reference" => reference = args.next().map(PathBuf::from),
            "--drafts" => drafts = true,
            "--realtime" => realtime = true,
            "--speed" => speed = args.next().and_then(|s| s.parse().ok()).unwrap_or(1.0),
            "--relay" => expose_relay = true,
            "--repeat" => {
                repeat = args
                    .next()
                    .expect("repeat count")
                    .parse()
                    .expect("repeat count")
            }
            "--chunk-samples" => {
                chunk_sizes = args
                    .next()
                    .expect("chunk sizes")
                    .split(',')
                    .map(|value| value.parse::<usize>().expect("positive chunk size"))
                    .collect();
                assert!(chunk_sizes.iter().all(|count| *count > 0));
            }
            other => eprintln!("ignoring unknown argument {other}"),
        }
    }
    let wav = wav.expect("usage: replay_sidecar --wav PATH [--realtime] [--drafts] [--speed N]");
    assert!(speed.is_finite() && speed > 0.0 && repeat > 0 && repeat <= 1000);

    // Isolated HOME: the sidecar writes live-transcript.jsonl, its status file,
    // and minutes.log under ~/.minutes, and must never touch the real ones.
    let mut config = minutes_core::config::Config::load();
    // Resolve the real model directory before isolating output. No symlinks,
    // administrator privileges, or copies of multi-gigabyte models needed.
    config.transcription.model_path =
        std::fs::canonicalize(&config.transcription.model_path).expect("model directory");
    let temp = tempfile::Builder::new()
        .prefix("replay-sidecar-")
        .tempdir()
        .expect("tempdir");
    let th = temp.path().to_path_buf();
    std::fs::create_dir_all(th.join(".minutes")).unwrap();
    std::fs::create_dir_all(th.join(".config")).unwrap();
    std::env::set_var("HOME", &th);
    std::env::remove_var("XDG_CONFIG_HOME");
    eprintln!("replay_home={}", th.display());

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("info,whisper_rs=warn,ggml=warn")
            }),
        )
        .with_writer(std::io::stderr)
        .init();
    minutes_core::install_whisper_logging_hooks();

    eprintln!(
        "config: live.model={:?} dictation.model={} partial_max_secs={} vad_engine={} effective_live_backend={}",
        config.live_transcript.model,
        config.dictation.model,
        config.transcription.partial_max_secs,
        config.transcription.vad_engine,
        config.effective_live_transcript_backend(),
    );

    let source = read_wav(&wav);
    let mut samples = Vec::new();
    for _ in 0..repeat {
        samples.extend_from_slice(&source);
        samples.resize(samples.len() + 16_000, 0.0);
    }
    let audio_secs = samples.len() as f64 / 16000.0;
    eprintln!("wav: {audio_secs:.1}s drafts={drafts} realtime={realtime} speed={speed}");

    let stop_flag = Arc::new(AtomicBool::new(false));
    let (publisher, mut subscriber) = if drafts {
        let (p, s) = minutes_core::live_partials::channel_with_source(
            1,
            minutes_core::live_partials::DEFAULT_PARTIAL_CHANNEL_CAPACITY,
            "recording-sidecar",
        );
        (Some(p), Some(s))
    } else {
        (None, None)
    };
    let relay = expose_relay.then(|| {
        minutes_core::copilot::CaptureRelayServer::start(
            minutes_core::copilot::CopilotEvidenceMode::CaptureRelayPartials,
            subscriber.take(),
        )
        .expect("isolated capture relay")
    });

    // Drain drafts like the capture relay would, counting them.
    let draft_events = Arc::new(AtomicU64::new(0));
    let drain_stop = Arc::new(AtomicBool::new(false));
    let drain = subscriber.map(|mut sub| {
        let seen = Arc::clone(&draft_events);
        let stop = Arc::clone(&drain_stop);
        std::thread::spawn(move || {
            let mut first_latencies = Vec::<u128>::new();
            let mut ages = Vec::<u128>::new();
            let mut prior_utterance = None;
            while !stop.load(Ordering::Relaxed) {
                while let Some(event) = sub.try_recv() {
                    if let minutes_core::live_partials::LivePartialEvent::Partial(partial) = event {
                        let age = partial.audio_snapshot_at.elapsed().as_millis();
                        if age <= 3000 {
                            seen.fetch_add(1, Ordering::Relaxed);
                            ages.push(age);
                            if prior_utterance != Some(partial.utterance_sequence) {
                                first_latencies
                                    .push(partial.audio_received_at.elapsed().as_millis());
                                prior_utterance = Some(partial.utterance_sequence);
                            }
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            first_latencies.sort_unstable();
            let p95 = (!first_latencies.is_empty())
                .then(|| first_latencies[(first_latencies.len() * 95).div_ceil(100) - 1]);
            eprintln!(
                "DRAFT_METRICS fresh_drafts={} first_draft_p95_ms={p95:?} max_age_ms={:?}",
                ages.len(),
                ages.iter().max()
            );
        })
    });

    let (tx, rx) = minutes_core::sidecar_audio::channel(200);
    let cfg = config.clone();
    let sf = Arc::clone(&stop_flag);
    let sidecar = std::thread::Builder::new()
        .name("live-sidecar".into())
        .spawn(move || minutes_core::live_transcript::run_sidecar_mpsc(rx, sf, &cfg, publisher))
        .unwrap();

    let started_wall = chrono::Local::now();
    let started = Instant::now();
    let mut sent = 0u64;
    let mut dropped = 0u64;
    let mut sample_offset = 0;
    let mut i = 0;
    while sample_offset < samples.len() {
        let end = (sample_offset + chunk_sizes[i % chunk_sizes.len()]).min(samples.len());
        let chunk = &samples[sample_offset..end];
        if realtime {
            let target = started + Duration::from_secs_f64(end as f64 / 16000.0 / speed);
            let now = Instant::now();
            if target > now {
                std::thread::sleep(target - now);
            }
            match tx.try_send(chunk.to_vec()) {
                Ok(()) => sent += 1,
                Err(std::sync::mpsc::TrySendError::Full(_)) => dropped += 1,
                Err(_) => break,
            }
        } else {
            if tx.send(chunk.to_vec()).is_err() {
                break;
            }
            sent += 1;
        }
        if i % 600 == 599 {
            eprintln!(
                "[feeder] audio_t={:.0}s wall={:.0}s sent={sent} dropped={dropped} draft_events={}",
                end as f64 / 16000.0,
                started.elapsed().as_secs_f64(),
                draft_events.load(Ordering::Relaxed)
            );
        }
        sample_offset = end;
        i += 1;
    }
    drop(tx);
    sidecar.join().expect("sidecar panicked");
    drain_stop.store(true, Ordering::Relaxed);
    if let Some(d) = drain {
        d.join().ok();
    }
    let wall = started.elapsed().as_secs_f64();

    let content =
        std::fs::read_to_string(th.join(".minutes/live-transcript.jsonl")).unwrap_or_default();
    let lines: Vec<minutes_core::live_transcript::TranscriptLine> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let words: usize = lines
        .iter()
        .map(|l| l.text.split_whitespace().count())
        .sum();
    let covered: f64 = lines.iter().map(|l| l.duration_ms as f64 / 1000.0).sum();
    if realtime {
        let mut latencies: Vec<i64> = lines
            .iter()
            .map(|line| {
                (line
                    .ts
                    .signed_duration_since(started_wall)
                    .num_milliseconds()
                    - ((line.offset_ms + line.duration_ms) as f64 / speed).round() as i64)
                    .max(0)
            })
            .collect();
        latencies.sort_unstable();
        let p95 =
            (!latencies.is_empty()).then(|| latencies[(latencies.len() * 95).div_ceil(100) - 1]);
        println!(
            "FINAL_METRICS source_end_to_final_p95_ms={p95:?} max_ms={:?}",
            latencies.last()
        );
    }
    if let Some(path) = reference {
        let expected = std::fs::read_to_string(path).expect("reference text");
        let expected = vec![expected; repeat].join(" ");
        let actual = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "ACCURACY normalized_wer_percent={:.2}",
            100.0 * word_error_rate(&expected, &actual)
        );
    }
    println!(
        "RESULT drafts={drafts} realtime={realtime} speed={speed} audio_s={audio_secs:.0} wall_s={wall:.0} chunks_sent={sent} chunks_dropped={dropped} draft_events={} final_lines={} final_words={words} covered_audio_s={covered:.0}",
        draft_events.load(Ordering::Relaxed),
        lines.len(),
    );
    for l in &lines {
        println!(
            "LINE {} off={}s dur={:.1}s {}",
            l.line,
            l.offset_ms / 1000,
            l.duration_ms as f64 / 1000.0,
            l.text.chars().take(90).collect::<String>()
        );
    }
    // The sidecar's own summary (live_sidecar_ended) lands in the temp HOME's
    // minutes.log; surface it so drops and whisper failures are in the report.
    if let Ok(log) = std::fs::read_to_string(th.join(".minutes/logs/minutes.log")) {
        for entry in log.lines().filter(|l| l.contains("live_sidecar_ended")) {
            println!("SUMMARY {entry}");
        }
    }
    let _ = stop_flag;
    drop(relay);
}

#[cfg(not(all(feature = "whisper", feature = "streaming")))]
fn main() {
    eprintln!("build with --features \"whisper streaming\"");
}
