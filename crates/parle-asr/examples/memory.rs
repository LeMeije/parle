//! Memory and load-time probe for ONE model, the evidence behind the app
//! freeing the model when idle (src-tauri/src/model_idle.rs). It answers:
//!
//! 1. What does the process hold while the model is loaded?
//! 2. What does a dictation add on top, at its peak?
//! 3. Does dropping the model really give the memory back to macOS?
//! 4. What does a RELOAD cost once the process has run the model once (the
//!    Metal device and compiled pipelines are process-wide and survive)?
//!
//! Flags, all optional, after the two positional arguments:
//!   --cycles N   repeat load, one pass, drop N times; prints the floor after
//!                each drop, so a per-cycle leak would show as a climb
//!   --settle S   after the last drop, print the footprint every 10 s for S s;
//!                macOS takes freed pages back lazily, so the floor right
//!                after a drop overstates what is still held
//!   --breakdown  macOS's own per-category account (`footprint`) at that point
//!   --cold       finally evict the model file from the OS file cache and time
//!                a reload that has to read it from the SSD
//!
//! Run (macOS):
//!   cargo run --release --example memory -p parle-asr --features metal -- <model.bin> <flash 0|1> [flags]

use parle_asr::whisper::WhisperEngine;
use parle_asr::{AsrEngine, TranscribeOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// (phys_footprint, lifetime max phys_footprint) in MB, from
/// proc_pid_rusage(RUSAGE_INFO_V4). phys_footprint is the figure Activity
/// Monitor shows in its Memory column.
#[cfg(target_os = "macos")]
fn mem() -> (f64, f64) {
    extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut u64) -> i32;
    }
    // rusage_info_v4 as u64 slots: uuid [0..2), phys_footprint [9],
    // lifetime_max_phys_footprint [30]. Oversized buffer on purpose.
    let mut buf = [0u64; 64];
    let rc = unsafe { proc_pid_rusage(std::process::id() as i32, 4, buf.as_mut_ptr()) };
    assert_eq!(rc, 0, "proc_pid_rusage failed");
    let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
    (mb(buf[9]), mb(buf[30]))
}

#[cfg(not(target_os = "macos"))]
fn mem() -> (f64, f64) {
    (0.0, 0.0)
}

fn line(label: &str) {
    let (now, peak) = mem();
    println!("  {label:<46} footprint {now:7.1} MB   peak {peak:7.1} MB");
}

/// Drop a file's pages from the unified buffer cache without root: map it and
/// msync(MS_INVALIDATE). The next read comes from the SSD.
#[cfg(target_os = "macos")]
fn evict_from_file_cache(path: &Path) {
    use std::ffi::c_void;
    use std::os::fd::AsRawFd;
    extern "C" {
        fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut c_void;
        fn msync(addr: *mut c_void, len: usize, flags: i32) -> i32;
        fn munmap(addr: *mut c_void, len: usize) -> i32;
    }
    const PROT_READ: i32 = 1;
    const MAP_SHARED: i32 = 1;
    const MS_INVALIDATE: i32 = 2;
    let file = std::fs::File::open(path).expect("open model");
    let len = file.metadata().expect("stat model").len() as usize;
    unsafe {
        let addr = mmap(std::ptr::null_mut(), len, PROT_READ, MAP_SHARED, file.as_raw_fd(), 0);
        assert_ne!(addr as isize, -1, "mmap failed");
        assert_eq!(msync(addr, len, MS_INVALIDATE), 0, "msync failed");
        munmap(addr, len);
    }
}

#[cfg(not(target_os = "macos"))]
fn evict_from_file_cache(_: &Path) {}

fn read_wav_16k(path: &Path) -> Vec<f32> {
    let data = std::fs::read(path).expect("fixture missing");
    let pos = data.windows(4).position(|w| w == b"data").expect("no data chunk");
    data[pos + 8..]
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
        .collect()
}

fn text_of(engine: &mut WhisperEngine, samples: &[f32]) -> (u128, String) {
    let t = Instant::now();
    let out = engine.transcribe(samples, &TranscribeOptions::default(), None).expect("transcribe");
    let ms = t.elapsed().as_millis();
    let text = out.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ");
    (ms, text)
}

fn flag_value(args: &[String], name: &str) -> Option<u64> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let model = PathBuf::from(args.get(1).expect("usage: memory <model.bin> <flash 0|1> [flags]"));
    let flash = args.get(2).map(|s| s == "1").unwrap_or(false);
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/fixtures");
    let hello = read_wav_16k(&fixtures.join("hello-16k.wav"));
    let email = read_wav_16k(&fixtures.join("email-16k.wav"));
    let id = model.file_stem().unwrap().to_string_lossy().to_string();
    let load = || WhisperEngine::load_with(&model, &id, true, true, flash).expect("load");

    println!("# memory probe: {id}, flash_attn={flash}");
    line("baseline (nothing loaded)");

    let t = Instant::now();
    let mut engine = load();
    println!("  first load in process (Metal init + read): {} ms", t.elapsed().as_millis());
    line("after load");

    let (ms, text_hello) = text_of(&mut engine, &hello);
    println!("  hello, first pass after first load: {ms} ms");
    let mut warm: Vec<u128> = (0..3).map(|_| text_of(&mut engine, &hello).0).collect();
    warm.sort();
    println!("  hello, warm passes: {warm:?} ms");
    let mut email_ms = Vec::new();
    let mut text_email = String::new();
    for _ in 0..2 {
        let (ms, t) = text_of(&mut engine, &email);
        email_ms.push(ms);
        text_email = t;
    }
    println!("  email (28.7 s), warm passes: {email_ms:?} ms");
    line("after dictations (peak = load + one dictation)");

    drop(engine);
    std::thread::sleep(Duration::from_millis(500));
    line("after DROP");

    let t = Instant::now();
    let mut engine = load();
    println!("  RELOAD, file cached, process warm: {} ms", t.elapsed().as_millis());
    line("after reload");
    let (ms1, _) = text_of(&mut engine, &hello);
    let (ms2, _) = text_of(&mut engine, &hello);
    println!("  hello after reload: first {ms1} ms, second {ms2} ms");
    drop(engine);
    std::thread::sleep(Duration::from_millis(500));
    line("after second DROP");

    for c in 1..=flag_value(&args, "--cycles").unwrap_or(0) {
        let t = Instant::now();
        let mut engine = load();
        let load_ms = t.elapsed().as_millis();
        let (ms, _) = text_of(&mut engine, &hello);
        drop(engine);
        std::thread::sleep(Duration::from_millis(300));
        line(&format!("cycle {c}: load {load_ms} ms, pass {ms} ms, after drop"));
    }

    if let Some(secs) = flag_value(&args, "--settle") {
        for t in (10..=secs).step_by(10) {
            std::thread::sleep(Duration::from_secs(10));
            line(&format!("idle +{t} s"));
        }
    }

    if args.iter().any(|a| a == "--breakdown") {
        let out = std::process::Command::new("footprint")
            .arg(std::process::id().to_string())
            .output()
            .expect("footprint");
        println!("{}", String::from_utf8_lossy(&out.stdout).lines().take(22).collect::<Vec<_>>().join("\n"));
    }

    if args.iter().any(|a| a == "--cold") {
        evict_from_file_cache(&model);
        let t = Instant::now();
        let mut engine = load();
        println!("  RELOAD, file evicted from the OS cache (read from SSD): {} ms", t.elapsed().as_millis());
        let (ms, _) = text_of(&mut engine, &hello);
        println!("  hello after cold reload: {ms} ms");
    }

    println!("\n  text hello: {text_hello}");
    println!("  text email: {text_email}");
}
