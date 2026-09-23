//! `bulkload-bench micro <name>`: M0 micro-benchmarks (M2 W2, TIN-4541).
//!
//! Each item isolates one cost the M2 design budgets against, so every later
//! design number is measured rather than assumed:
//!
//! - `cdc`: `FastCDC` throughput, `StreamCDC` as the agent uses it today versus
//!   the slice cutter (`FastCDC::cut`) over an in-memory buffer.
//! - `blake3`: single-call BLAKE3 over 16 KiB, 64 KiB and 256 KiB chunks.
//! - `socket`: `AF_UNIX` socketpair throughput at default buffers and at
//!   4 MiB `SO_SNDBUF`/`SO_RCVBUF`.
//! - `flush`: `fsync`, `F_BARRIERFSYNC` and `F_FULLFSYNC` cost on `--dir` as a
//!   function of dirty bytes (1, 16, 64, 256 MiB).
//! - `durable`: `pwrite` of `--bytes` (default the R23 corpus size) plus
//!   `F_BARRIERFSYNC` or `F_FULLFSYNC` on `--dir`: the durable-write floor.
//!
//! CPU items (`cdc`, `blake3`, `socket`) run once per core class. On Darwin a
//! thread asks for P-cores with `QOS_CLASS_USER_INTERACTIVE` and for E-cores
//! with `QOS_CLASS_BACKGROUND` via `pthread_set_qos_class_self_np`; the
//! scheduler, not the benchmark, places the thread. Aggregate rows run one
//! thread per core of the class (`--p-cores`, `--e-cores`).
//!
//! Output is one `micro …` `key=value` line per repetition and one
//! `micro_median …` line per configuration. Scratch files are created only
//! under `--dir` and unlinked after each measurement.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::{FileExt as _, OpenOptionsExt as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Barrier;
use std::time::{Duration, Instant};

use bulkload_agent::hash::{CDC_AVG_BYTES, CDC_MAX_BYTES, CDC_MIN_BYTES};
use clap::{Parser, ValueEnum};

const MIB: u64 = 1024 * 1024;
const R23_CORPUS_BYTES: u64 = 242_605_606;
const WRITE_BLOCK: usize = 8 * 1024 * 1024;
const SOCKET_IO: usize = 256 * 1024;
const SOCKET_BUFFER: libc::c_int = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Name {
    Cdc,
    Blake3,
    Socket,
    Flush,
    Durable,
}

/// Core class requested through Darwin `QoS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Class {
    /// `QOS_CLASS_USER_INTERACTIVE`: the scheduler prefers P-cores.
    P,
    /// `QOS_CLASS_BACKGROUND`: the scheduler confines the thread to E-cores.
    E,
}

impl Class {
    const fn label(self) -> &'static str {
        match self {
            Self::P => "p",
            Self::E => "e",
        }
    }

    const fn qos_label(self) -> &'static str {
        match self {
            Self::P => "user-interactive",
            Self::E => "background",
        }
    }
}

/// M0 micro-benchmarks. Scratch lives only under --dir.
#[derive(Debug, Parser)]
#[command(name = "bulkload-bench micro")]
struct MicroCli {
    /// Which micro-benchmark to run.
    #[arg(value_enum)]
    name: Name,
    /// Existing directory on the volume under test (flush, durable).
    #[arg(long)]
    dir: Option<PathBuf>,
    /// Read-only input file to load into memory (cdc, blake3). Default: random.
    #[arg(long)]
    input: Option<PathBuf>,
    /// Payload bytes: in-memory buffer (cdc, blake3, socket) or file (durable).
    #[arg(long)]
    bytes: Option<u64>,
    /// Repetitions per configuration.
    #[arg(long, default_value_t = 3)]
    reps: usize,
    /// Core classes to run CPU items on.
    #[arg(long, value_enum, num_args = 1.., default_values_t = [Class::P, Class::E])]
    class: Vec<Class>,
    /// Threads for the P-class aggregate row (neo, A18 Pro: 2).
    #[arg(long, default_value_t = 2)]
    p_cores: usize,
    /// Threads for the E-class aggregate row (neo, A18 Pro: 4).
    #[arg(long, default_value_t = 4)]
    e_cores: usize,
}

/// Run `bulkload-bench micro …`; `args` begins with the word `micro`.
///
/// # Errors
/// Returns argument, I/O and flush failures.
pub fn run(args: &[OsString]) -> io::Result<()> {
    let cli = MicroCli::try_parse_from(args)
        .map_err(|error| io::Error::other(error.render().to_string()))?;
    if cli.reps == 0 || cli.reps > 25 {
        return Err(io::Error::other("--reps must be 1..=25"));
    }
    match cli.name {
        Name::Cdc => cdc(&cli),
        Name::Blake3 => blake3_micro(&cli),
        Name::Socket => socket(&cli),
        Name::Flush => flush(&cli),
        Name::Durable => durable(&cli),
    }
}

// --- core-class placement --------------------------------------------------

#[cfg(target_vendor = "apple")]
mod qos {
    pub const USER_INTERACTIVE: u32 = 0x21;
    pub const BACKGROUND: u32 = 0x09;

    extern "C" {
        // <pthread/qos.h>; qos_class_t is an unsigned int enumeration.
        pub fn pthread_set_qos_class_self_np(
            qos_class: u32,
            relative_priority: libc::c_int,
        ) -> libc::c_int;
    }
}

/// Ask the scheduler to place the calling thread on `class` cores.
#[cfg(target_vendor = "apple")]
fn place_self(class: Class) -> io::Result<()> {
    let qos = match class {
        Class::P => qos::USER_INTERACTIVE,
        Class::E => qos::BACKGROUND,
    };
    // SAFETY: the call only changes the calling thread's own QoS; both
    // arguments are plain integers, and relative priority 0 is always valid.
    let result = unsafe { qos::pthread_set_qos_class_self_np(qos, 0) };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result));
    }
    Ok(())
}

#[cfg(not(target_vendor = "apple"))]
const fn place_self(_: Class) -> io::Result<()> {
    Ok(())
}

/// CPU time consumed so far by the calling thread.
///
/// Rates over thread CPU time are insensitive to time-slicing by other load,
/// so they estimate per-core throughput on a contended host.
fn thread_cpu() -> io::Result<Duration> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a live, writable timespec for the duration of the call,
    // and CLOCK_THREAD_CPUTIME_ID is a valid clock on Darwin and Linux.
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut now) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let seconds = u64::try_from(now.tv_sec).map_err(io::Error::other)?;
    let nanos = u32::try_from(now.tv_nsec).map_err(io::Error::other)?;
    Ok(Duration::new(seconds, nanos))
}

/// Bytes processed, wall time, and CPU time summed over the worker threads.
struct Measured {
    bytes: u64,
    wall: Duration,
    cpu: Duration,
}

/// Run `work` on `threads` threads placed on `class`.
///
/// Wall timing starts once every thread is placed and waiting at the barrier.
fn on_class(
    class: Class,
    threads: usize,
    work: &(dyn Fn() -> io::Result<u64> + Sync),
) -> io::Result<Measured> {
    let barrier = Barrier::new(threads + 1);
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(threads);
        for _ in 0..threads {
            let barrier = &barrier;
            handles.push(scope.spawn(move || -> io::Result<(u64, Duration)> {
                let placed = place_self(class);
                barrier.wait();
                placed?;
                let before = thread_cpu()?;
                let bytes = work()?;
                Ok((bytes, thread_cpu()?.saturating_sub(before)))
            }));
        }
        barrier.wait();
        let started = Instant::now();
        let mut total = 0_u64;
        let mut cpu = Duration::ZERO;
        let mut failure = None;
        for handle in handles {
            match handle.join() {
                Ok(Ok((bytes, spent))) => {
                    total = total.saturating_add(bytes);
                    cpu = cpu.saturating_add(spent);
                }
                Ok(Err(error)) => failure = Some(error),
                Err(_) => failure = Some(io::Error::other("micro worker panicked")),
            }
        }
        let wall = started.elapsed();
        failure.map_or(
            Ok(Measured {
                bytes: total,
                wall,
                cpu,
            }),
            Err,
        )
    })
}

// --- reporting -------------------------------------------------------------

#[allow(clippy::cast_precision_loss)]
fn gb_per_s(bytes: u64, elapsed: Duration) -> f64 {
    let seconds = elapsed.as_secs_f64();
    if seconds == 0.0 {
        0.0
    } else {
        bytes as f64 / seconds / 1e9
    }
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    let high = values.get(middle).copied().unwrap_or(0.0);
    if values.len().is_multiple_of(2) {
        let low = values
            .get(middle.saturating_sub(1))
            .copied()
            .unwrap_or(high);
        f64::midpoint(low, high)
    } else {
        high
    }
}

/// One CPU configuration: `reps` samples, one line each, then the median.
fn cpu_rows(
    cli: &MicroCli,
    name: &str,
    variant: &str,
    work: &(dyn Fn() -> io::Result<u64> + Sync),
) -> io::Result<()> {
    for class in &cli.class {
        let aggregate = match class {
            Class::P => cli.p_cores,
            Class::E => cli.e_cores,
        };
        let mut shapes = vec![1];
        if aggregate > 1 {
            shapes.push(aggregate);
        }
        for threads in shapes {
            let mut rates = Vec::with_capacity(cli.reps);
            let mut cpu_rates = Vec::with_capacity(cli.reps);
            for rep in 0..cli.reps {
                let measured = on_class(*class, threads, work)?;
                let rate = gb_per_s(measured.bytes, measured.wall);
                // Per-thread rate over CPU time actually received.
                let cpu_rate = gb_per_s(measured.bytes, measured.cpu);
                rates.push(rate);
                cpu_rates.push(cpu_rate);
                println!(
                    "micro name={name} variant={variant} core_class={} qos={} threads={threads} rep={rep} bytes={} elapsed_ns={} cpu_ns={} gb_s={rate:.3} cpu_gb_s_per_thread={cpu_rate:.3}",
                    class.label(),
                    class.qos_label(),
                    measured.bytes,
                    measured.wall.as_nanos(),
                    measured.cpu.as_nanos(),
                );
            }
            println!(
                "micro_median name={name} variant={variant} core_class={} qos={} threads={threads} reps={} gb_s={:.3} cpu_gb_s_per_thread={:.3}",
                class.label(),
                class.qos_label(),
                cli.reps,
                median(&mut rates),
                median(&mut cpu_rates),
            );
        }
    }
    Ok(())
}

// --- inputs ----------------------------------------------------------------

/// Deterministic incompressible bytes (xorshift64*), like the git-pack corpus.
fn random_bytes(length: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut bytes = Vec::with_capacity(length + 8);
    while bytes.len() < length {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        bytes.extend_from_slice(&state.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes());
    }
    bytes.truncate(length);
    bytes
}

fn usize_bytes(bytes: u64) -> io::Result<usize> {
    usize::try_from(bytes).map_err(io::Error::other)
}

fn input_buffer(cli: &MicroCli) -> io::Result<Vec<u8>> {
    if let Some(path) = &cli.input {
        let mut data = Vec::new();
        File::open(path)?.read_to_end(&mut data)?;
        println!(
            "micro_input source=file path={} bytes={}",
            path.display(),
            data.len()
        );
        return Ok(data);
    }
    let length = usize_bytes(cli.bytes.unwrap_or(256 * MIB))?;
    println!("micro_input source=random-xorshift bytes={length}");
    Ok(random_bytes(length, 0x5eed_b01c_10ad_f00d))
}

// --- (a) CDC -----------------------------------------------------------------

fn cdc(cli: &MicroCli) -> io::Result<()> {
    let data = input_buffer(cli)?;
    let stream = || -> io::Result<u64> {
        let mut total = 0_u64;
        for chunk in fastcdc::v2020::StreamCDC::new(
            data.as_slice(),
            CDC_MIN_BYTES,
            CDC_AVG_BYTES,
            CDC_MAX_BYTES,
        ) {
            let chunk = chunk.map_err(io::Error::other)?;
            total = total.saturating_add(chunk.data.len() as u64);
        }
        Ok(total)
    };
    let slice = || -> io::Result<u64> {
        let chunker =
            fastcdc::v2020::FastCDC::new(&data, CDC_MIN_BYTES, CDC_AVG_BYTES, CDC_MAX_BYTES);
        let mut start = 0;
        while start < data.len() {
            let (_, end) = chunker.cut(start, data.len() - start);
            if end <= start {
                return Err(io::Error::other("FastCDC::cut made no progress"));
            }
            start = end;
        }
        Ok(start as u64)
    };
    cpu_rows(cli, "cdc", "stream-cdc", &stream)?;
    cpu_rows(cli, "cdc", "slice-cut", &slice)
}

// --- (b) BLAKE3 --------------------------------------------------------------

fn blake3_micro(cli: &MicroCli) -> io::Result<()> {
    let data = input_buffer(cli)?;
    for size in [16 * 1024, 64 * 1024, 256 * 1024] {
        let work = || -> io::Result<u64> {
            let mut total = 0_u64;
            let mut fold = 0_u8;
            for chunk in data.chunks(size) {
                fold ^= blake3::hash(chunk).as_bytes().first().copied().unwrap_or(0);
                total = total.saturating_add(chunk.len() as u64);
            }
            std::hint::black_box(fold);
            Ok(total)
        };
        cpu_rows(cli, "blake3", &format!("chunk-{}k", size / 1024), &work)?;
    }
    Ok(())
}

// --- (d) AF_UNIX socketpair --------------------------------------------------

fn set_buffer(stream: &UnixStream, option: libc::c_int, bytes: libc::c_int) -> io::Result<()> {
    let length =
        libc::socklen_t::try_from(std::mem::size_of::<libc::c_int>()).map_err(io::Error::other)?;
    // SAFETY: the descriptor is owned by `stream` for the call; the option
    // value points at a live c_int whose exact size is passed as its length.
    let result = unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            option,
            std::ptr::from_ref(&bytes).cast(),
            length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn get_buffer(stream: &UnixStream, option: libc::c_int) -> io::Result<libc::c_int> {
    let mut value: libc::c_int = 0;
    let mut length =
        libc::socklen_t::try_from(std::mem::size_of::<libc::c_int>()).map_err(io::Error::other)?;
    // SAFETY: the descriptor is owned by `stream`; `value` and `length` are
    // live, writable and correctly sized for a c_int option.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            option,
            std::ptr::from_mut(&mut value).cast(),
            &raw mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(value)
}

fn socket_once(class: Class, total: usize, buffer: Option<libc::c_int>) -> io::Result<Measured> {
    let (mut writer, mut reader) = UnixStream::pair()?;
    if let Some(bytes) = buffer {
        for stream in [&writer, &reader] {
            set_buffer(stream, libc::SO_SNDBUF, bytes)?;
            set_buffer(stream, libc::SO_RCVBUF, bytes)?;
        }
    }
    let payload = random_bytes(SOCKET_IO, 0x50c4);
    let barrier = Barrier::new(3);
    std::thread::scope(|scope| -> io::Result<Measured> {
        let barrier = &barrier;
        let payload = &payload;
        let writing = scope.spawn(move || -> io::Result<Duration> {
            let placed = place_self(class);
            barrier.wait();
            placed?;
            let before = thread_cpu()?;
            let mut remaining = total;
            while remaining > 0 {
                let count = remaining.min(payload.len());
                writer.write_all(payload.get(..count).unwrap_or(payload))?;
                remaining -= count;
            }
            Ok(thread_cpu()?.saturating_sub(before))
        });
        let reading = scope.spawn(move || -> io::Result<Duration> {
            let placed = place_self(class);
            let mut sink = vec![0_u8; SOCKET_IO];
            barrier.wait();
            placed?;
            let before = thread_cpu()?;
            let mut remaining = total;
            while remaining > 0 {
                let read = reader.read(&mut sink)?;
                if read == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                remaining = remaining.saturating_sub(read);
            }
            Ok(thread_cpu()?.saturating_sub(before))
        });
        barrier.wait();
        let started = Instant::now();
        let sent = writing
            .join()
            .map_err(|_| io::Error::other("sender panicked"));
        let drained = reading
            .join()
            .map_err(|_| io::Error::other("receiver panicked"));
        let wall = started.elapsed();
        let cpu = sent??.saturating_add(drained??);
        Ok(Measured {
            bytes: total as u64,
            wall,
            cpu,
        })
    })
}

fn socket(cli: &MicroCli) -> io::Result<()> {
    let total = usize_bytes(cli.bytes.unwrap_or(1024 * MIB))?;
    let (probe, _peer) = UnixStream::pair()?;
    println!(
        "micro_socket default_sndbuf={} default_rcvbuf={} io_bytes={SOCKET_IO}",
        get_buffer(&probe, libc::SO_SNDBUF)?,
        get_buffer(&probe, libc::SO_RCVBUF)?,
    );
    drop(probe);
    for (variant, buffer) in [
        ("default-buffers", None),
        ("4mib-buffers", Some(SOCKET_BUFFER)),
    ] {
        if let Some(bytes) = buffer {
            let (probe, _peer) = UnixStream::pair()?;
            set_buffer(&probe, libc::SO_SNDBUF, bytes)?;
            set_buffer(&probe, libc::SO_RCVBUF, bytes)?;
            println!(
                "micro_socket variant={variant} effective_sndbuf={} effective_rcvbuf={}",
                get_buffer(&probe, libc::SO_SNDBUF)?,
                get_buffer(&probe, libc::SO_RCVBUF)?,
            );
        }
        for class in &cli.class {
            let mut rates = Vec::with_capacity(cli.reps);
            for rep in 0..cli.reps {
                let measured = socket_once(*class, total, buffer)?;
                let rate = gb_per_s(measured.bytes, measured.wall);
                rates.push(rate);
                println!(
                    "micro name=socket variant={variant} core_class={} qos={} threads=2 rep={rep} bytes={total} elapsed_ns={} cpu_ns_both_threads={} gb_s={rate:.3}",
                    class.label(),
                    class.qos_label(),
                    measured.wall.as_nanos(),
                    measured.cpu.as_nanos(),
                );
            }
            println!(
                "micro_median name=socket variant={variant} core_class={} qos={} threads=2 reps={} gb_s={:.3}",
                class.label(),
                class.qos_label(),
                cli.reps,
                median(&mut rates),
            );
        }
    }
    Ok(())
}

// --- (c) and (e): flushes on a volume --------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flush {
    None,
    Fsync,
    Barrier,
    Full,
}

impl Flush {
    /// Flush kinds this platform can issue: Darwin adds the two fcntl flushes.
    fn available() -> Vec<Self> {
        let mut kinds = vec![Self::Fsync];
        if cfg!(target_vendor = "apple") {
            kinds.extend([Self::Barrier, Self::Full]);
        }
        kinds
    }

    const fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Fsync => "fsync",
            Self::Barrier => "f_barrierfsync",
            Self::Full => "f_fullfsync",
        }
    }
}

/// Issue exactly one flush of `kind` on `file`.
fn flush_once(file: &File, kind: Flush) -> io::Result<()> {
    let fd = file.as_raw_fd();
    let result = match kind {
        Flush::None => 0,
        // SAFETY: `fd` is owned by `file`, which outlives the call.
        Flush::Fsync => unsafe { libc::fsync(fd) },
        // SAFETY: as above; F_BARRIERFSYNC (85) takes no argument.
        #[cfg(target_vendor = "apple")]
        Flush::Barrier => unsafe { libc::fcntl(fd, libc::F_BARRIERFSYNC) },
        // SAFETY: as above; F_FULLFSYNC (51) takes no argument.
        #[cfg(target_vendor = "apple")]
        Flush::Full => unsafe { libc::fcntl(fd, libc::F_FULLFSYNC) },
        #[cfg(not(target_vendor = "apple"))]
        Flush::Barrier | Flush::Full => {
            return Err(io::Error::other(
                "F_BARRIERFSYNC/F_FULLFSYNC are Darwin-only",
            ))
        }
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn volume_dir(cli: &MicroCli) -> io::Result<PathBuf> {
    let dir = cli
        .dir
        .as_ref()
        .ok_or_else(|| io::Error::other("--dir is required for flush and durable"))?;
    let dir = fs::canonicalize(dir)?;
    if !dir.is_dir() {
        return Err(io::Error::other("--dir must be an existing directory"));
    }
    Ok(dir)
}

fn scratch_file(dir: &Path, name: &str) -> io::Result<(PathBuf, File)> {
    let path = dir.join(format!(".bulkload-micro-{}-{name}", std::process::id()));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    Ok((path, file))
}

/// `pwrite` `length` bytes from `block`, repeated; returns the elapsed time.
fn pwrite_all(file: &File, block: &[u8], length: u64) -> io::Result<Duration> {
    let started = Instant::now();
    let mut offset = 0_u64;
    while offset < length {
        let count = usize_bytes((length - offset).min(block.len() as u64))?;
        file.write_all_at(block.get(..count).unwrap_or(block), offset)?;
        offset += count as u64;
    }
    Ok(started.elapsed())
}

/// Drain earlier dirty state on the volume so each sample starts clean.
fn drain(dir: &Path) -> io::Result<()> {
    let (path, file) = scratch_file(dir, "drain")?;
    let flushed = flush_once(
        &file,
        Flush::available().last().copied().unwrap_or(Flush::Fsync),
    );
    fs::remove_file(path)?;
    flushed
}

struct VolumeSample {
    write: Duration,
    flush: Duration,
}

fn volume_sample(dir: &Path, block: &[u8], length: u64, kind: Flush) -> io::Result<VolumeSample> {
    drain(dir)?;
    let (path, file) = scratch_file(dir, kind.label())?;
    let measured = (|| -> io::Result<VolumeSample> {
        let write = pwrite_all(&file, block, length)?;
        let started = Instant::now();
        flush_once(&file, kind)?;
        Ok(VolumeSample {
            write,
            flush: started.elapsed(),
        })
    })();
    drop(file);
    fs::remove_file(path)?;
    measured
}

#[allow(clippy::cast_precision_loss)]
fn millis(duration: Duration) -> f64 {
    duration.as_nanos() as f64 / 1e6
}

fn flush(cli: &MicroCli) -> io::Result<()> {
    let dir = volume_dir(cli)?;
    let block = random_bytes(WRITE_BLOCK, 0xf1a5);
    println!("micro_volume name=flush dir={}", dir.display());
    for size_mib in [1_u64, 16, 64, 256] {
        for kind in Flush::available() {
            let mut flushes = Vec::with_capacity(cli.reps);
            for rep in 0..cli.reps {
                let sample = volume_sample(&dir, &block, size_mib * MIB, kind)?;
                flushes.push(millis(sample.flush));
                println!(
                    "micro name=flush kind={} dirty_mib={size_mib} rep={rep} write_ms={:.3} flush_ms={:.3}",
                    kind.label(),
                    millis(sample.write),
                    millis(sample.flush),
                );
            }
            println!(
                "micro_median name=flush kind={} dirty_mib={size_mib} reps={} flush_ms={:.3}",
                kind.label(),
                cli.reps,
                median(&mut flushes),
            );
        }
    }
    Ok(())
}

fn durable(cli: &MicroCli) -> io::Result<()> {
    let dir = volume_dir(cli)?;
    let length = cli.bytes.unwrap_or(R23_CORPUS_BYTES);
    let block = random_bytes(WRITE_BLOCK, 0xd00a);
    println!(
        "micro_volume name=durable dir={} bytes={length} write_block={WRITE_BLOCK}",
        dir.display()
    );
    for kind in std::iter::once(Flush::None).chain(Flush::available()) {
        let mut totals = Vec::with_capacity(cli.reps);
        for rep in 0..cli.reps {
            let sample = volume_sample(&dir, &block, length, kind)?;
            let total = sample.write + sample.flush;
            totals.push(millis(total));
            println!(
                "micro name=durable variant=pwrite+{} bytes={length} rep={rep} write_ms={:.3} flush_ms={:.3} total_ms={:.3} mb_s={:.1}",
                kind.label(),
                millis(sample.write),
                millis(sample.flush),
                millis(total),
                gb_per_s(length, total) * 1000.0,
            );
        }
        println!(
            "micro_median name=durable variant=pwrite+{} bytes={length} reps={} total_ms={:.3}",
            kind.label(),
            cli.reps,
            median(&mut totals),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn cpu_items_run_small() -> io::Result<()> {
        for name in ["cdc", "blake3", "socket"] {
            run(&args(&[
                "micro", name, "--bytes", "1048576", "--reps", "1", "--class", "p",
            ]))?;
        }
        Ok(())
    }

    #[test]
    fn volume_items_run_small_and_leave_no_scratch() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir
            .path()
            .to_str()
            .ok_or_else(|| io::Error::other("path"))?;
        run(&args(&[
            "micro", "durable", "--dir", path, "--bytes", "65536", "--reps", "1",
        ]))?;
        assert_eq!(fs::read_dir(dir.path())?.count(), 0);
        assert!(run(&args(&["micro", "flush"])).is_err());
        Ok(())
    }

    #[test]
    fn median_and_random_are_stable() {
        assert!((median(&mut [3.0, 1.0, 2.0]) - 2.0).abs() < f64::EPSILON);
        assert!((median(&mut [4.0, 1.0, 2.0, 3.0]) - 2.5).abs() < f64::EPSILON);
        assert_eq!(random_bytes(64, 7), random_bytes(64, 7));
        assert_eq!(random_bytes(13, 7).len(), 13);
    }
}
