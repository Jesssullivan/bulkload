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
//!   `F_BARRIERFSYNC` or `F_FULLFSYNC` on `--dir`: the single-file,
//!   single-flush durable-write floor. It repeats one 8 MiB block.
//! - `durable-corpus`: the corpus-shaped floor (R-N87). One file per regular
//!   file of `--source`, at the real sizes, each written with `pwrite` and
//!   flushed on its own (`F_FULLFSYNC`, or the `F_BARRIERFSYNC` variant), then
//!   every created directory and the parent are flushed with the same kind.
//!   Variants are interleaved within each repetition, in an order that
//!   rotates per repetition. `--read-source` includes reading the source
//!   bytes; without it the writer repeats one 8 MiB block. Source page-cache
//!   residency (`mincore`) is reported before every sample. Whether
//!   `F_FULLFSYNC` reaches media on a given enclosure is not proven by this
//!   benchmark.
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
use std::os::unix::fs::{DirBuilderExt as _, FileExt as _, OpenOptionsExt as _};
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
    DurableCorpus,
}

/// Per-file flush kind for `durable-corpus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum CorpusVariant {
    /// `F_FULLFSYNC` per file and per directory.
    Full,
    /// `F_BARRIERFSYNC` per file and per directory.
    Barrier,
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
    /// Corpus whose regular-file sizes shape `durable-corpus` (read-only).
    #[arg(long)]
    source: Option<PathBuf>,
    /// `durable-corpus`: read the source bytes and write them (timed).
    #[arg(long)]
    read_source: bool,
    /// `durable-corpus` variants, interleaved within each repetition.
    #[arg(long, value_enum, num_args = 1.., default_values_t = [CorpusVariant::Full, CorpusVariant::Barrier])]
    variant: Vec<CorpusVariant>,
    /// `durable-corpus` writer threads; files are striped across them.
    #[arg(long, default_value_t = 1)]
    jobs: usize,
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
        Name::DurableCorpus => durable_corpus(&cli),
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
///
/// One signature on every platform: the status is chosen by `cfg` inside the
/// function and converted by [`placed`]. Other platforms have no core-class
/// request, so their status is always 0.
fn place_self(class: Class) -> io::Result<()> {
    #[cfg(target_vendor = "apple")]
    let status = {
        let qos = match class {
            Class::P => qos::USER_INTERACTIVE,
            Class::E => qos::BACKGROUND,
        };
        // SAFETY: the call only changes the calling thread's own QoS; both
        // arguments are plain integers, and relative priority 0 is always valid.
        unsafe { qos::pthread_set_qos_class_self_np(qos, 0) }
    };
    #[cfg(not(target_vendor = "apple"))]
    let status = {
        let _ = class;
        0
    };
    placed(status)
}

/// Convert a [`place_self`] status into a result.
fn placed(status: libc::c_int) -> io::Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status))
    }
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
                let placement = place_self(class);
                barrier.wait();
                placement?;
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
            let placement = place_self(class);
            barrier.wait();
            placement?;
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
            let placement = place_self(class);
            let mut sink = vec![0_u8; SOCKET_IO];
            barrier.wait();
            placement?;
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

// --- (e') corpus-shaped durable floor (R-N87) ------------------------------

/// One regular file of the shaping corpus: relative path and size.
struct Shape {
    relative: PathBuf,
    size: u64,
}

/// Regular files under `root`, sorted by path. Only metadata is read.
fn corpus_shape(root: &Path) -> io::Result<(Vec<Shape>, Vec<PathBuf>)> {
    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        for entry in fs::read_dir(root.join(&relative))? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let child = relative.join(entry.file_name());
            if kind.is_dir() {
                directories.push(child.clone());
                pending.push(child);
            } else if kind.is_file() {
                files.push(Shape {
                    relative: child,
                    size: entry.metadata()?.len(),
                });
            }
        }
    }
    files.sort_by(|left, right| left.relative.cmp(&right.relative));
    directories.sort();
    Ok((files, directories))
}

/// Resident and total pages of `path` in the page cache (`mincore`).
fn residency(path: &Path) -> io::Result<(u64, u64)> {
    let file = File::open(path)?;
    let length = usize_bytes(file.metadata()?.len())?;
    if length == 0 {
        return Ok((0, 0));
    }
    // SAFETY: sysconf has no memory effects; _SC_PAGESIZE is always valid.
    let page =
        usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).map_err(io::Error::other)?;
    let pages = length.div_ceil(page.max(1));
    let mut vector = vec![0_u8; pages];
    // SAFETY: a read-only shared mapping of an open descriptor for exactly its
    // length. Nothing reads or writes through it; it is unmapped below.
    let address = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            length,
            libc::PROT_READ,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        )
    };
    if address == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `address`/`length` describe the live mapping created above, and
    // `vector` holds one writable byte per page of it. mincore faults nothing in.
    let status = unsafe { libc::mincore(address, length, vector.as_mut_ptr().cast()) };
    let queried = if status == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    };
    // SAFETY: unmaps exactly the mapping created above; it is not used again.
    unsafe { libc::munmap(address, length) };
    queried?;
    let resident = vector.iter().filter(|byte| **byte & 1 != 0).count();
    Ok((resident as u64, pages as u64))
}

/// Page-cache residency of every shaping file, as a fraction.
fn corpus_residency(source: &Path, files: &[Shape]) -> io::Result<f64> {
    let mut resident = 0_u64;
    let mut total = 0_u64;
    for shape in files {
        let (hit, pages) = residency(&source.join(&shape.relative))?;
        resident += hit;
        total += pages;
    }
    #[allow(clippy::cast_precision_loss)]
    Ok(if total == 0 {
        1.0
    } else {
        resident as f64 / total as f64
    })
}

#[derive(Default)]
struct CorpusTimes {
    read: Duration,
    write: Duration,
    file_flush: Duration,
}

/// Write (and optionally read) the files assigned to one writer, flushing each.
fn write_corpus_files(
    source: &Path,
    destination: &Path,
    files: &[&Shape],
    block: &[u8],
    read_source: bool,
    kind: Flush,
) -> io::Result<CorpusTimes> {
    let mut times = CorpusTimes::default();
    let mut buffer = vec![0_u8; WRITE_BLOCK];
    for shape in files {
        let mut input = if read_source {
            Some(File::open(source.join(&shape.relative))?)
        } else {
            None
        };
        let output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(destination.join(&shape.relative))?;
        let mut offset = 0_u64;
        while offset < shape.size {
            let count = usize_bytes((shape.size - offset).min(WRITE_BLOCK as u64))?;
            let data = if let Some(input) = input.as_mut() {
                let started = Instant::now();
                let slice = buffer
                    .get_mut(..count)
                    .ok_or_else(|| io::Error::other("read buffer bounds"))?;
                input.read_exact(slice)?;
                times.read += started.elapsed();
                &*slice
            } else {
                block.get(..count).unwrap_or(block)
            };
            let started = Instant::now();
            output.write_all_at(data, offset)?;
            times.write += started.elapsed();
            offset += count as u64;
        }
        let started = Instant::now();
        flush_once(&output, kind)?;
        times.file_flush += started.elapsed();
    }
    Ok(times)
}

struct CorpusSample {
    times: CorpusTimes,
    dir_flush: Duration,
    total: Duration,
}

fn corpus_sample(
    cli: &MicroCli,
    source: &Path,
    shape: &(Vec<Shape>, Vec<PathBuf>),
    root: &Path,
    kind: Flush,
) -> io::Result<CorpusSample> {
    let (files, directories) = shape;
    let block = random_bytes(WRITE_BLOCK, 0xc0de);
    let jobs = cli.jobs.clamp(1, 16);
    let started = Instant::now();
    fs::DirBuilder::new().mode(0o700).create(root)?;
    for directory in directories {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(root.join(directory))?;
    }
    let times = std::thread::scope(|scope| -> io::Result<CorpusTimes> {
        let mut handles = Vec::with_capacity(jobs);
        for job in 0..jobs {
            let assigned: Vec<&Shape> = files.iter().skip(job).step_by(jobs).collect();
            let block = &block;
            handles.push(scope.spawn(move || {
                write_corpus_files(source, root, &assigned, block, cli.read_source, kind)
            }));
        }
        let mut sum = CorpusTimes::default();
        let mut failure = None;
        for handle in handles {
            match handle.join() {
                Ok(Ok(times)) => {
                    sum.read += times.read;
                    sum.write += times.write;
                    sum.file_flush += times.file_flush;
                }
                Ok(Err(error)) => failure = Some(error),
                Err(_) => failure = Some(io::Error::other("corpus writer panicked")),
            }
        }
        failure.map_or(Ok(sum), Err)
    })?;
    let flushing = Instant::now();
    for directory in directories.iter().rev() {
        flush_once(&File::open(root.join(directory))?, kind)?;
    }
    flush_once(&File::open(root)?, kind)?;
    flush_once(&File::open(root.parent().unwrap_or(root))?, kind)?;
    let dir_flush = flushing.elapsed();
    Ok(CorpusSample {
        times,
        dir_flush,
        total: started.elapsed(),
    })
}

fn summary(values: &mut [f64]) -> (f64, f64, f64) {
    values.sort_by(f64::total_cmp);
    let min = values.first().copied().unwrap_or(0.0);
    let max = values.last().copied().unwrap_or(0.0);
    (min, median(values), max)
}

fn durable_corpus(cli: &MicroCli) -> io::Result<()> {
    let dir = volume_dir(cli)?;
    let source = fs::canonicalize(
        cli.source
            .as_ref()
            .ok_or_else(|| io::Error::other("--source is required for durable-corpus"))?,
    )?;
    if dir.starts_with(&source) || source.starts_with(&dir) {
        return Err(io::Error::other("--dir and --source must be disjoint"));
    }
    let shape = corpus_shape(&source)?;
    let bytes = shape.0.iter().map(|file| file.size).sum::<u64>();
    let mut variants = cli.variant.clone();
    variants.dedup();
    let kinds: Vec<Flush> = variants
        .iter()
        .map(|variant| match variant {
            CorpusVariant::Full => Flush::Full,
            CorpusVariant::Barrier => Flush::Barrier,
        })
        .collect();
    println!(
        "micro_assumptions name=durable-corpus dir={} source={} files={} directories={} bytes={bytes} read_source={} jobs={} write_block={WRITE_BLOCK} payload={} page_cache=not-dropped dest=fresh-directory-per-sample drain=untimed-f_fullfsync-before-sample file_flush=per-file dir_flush=every-created-directory-plus-parent order=rotating-per-rep cleanup=untimed-unlink media_durability_of_f_fullfsync=unproven-on-this-enclosure",
        dir.display(),
        source.display(),
        shape.0.len(),
        shape.1.len(),
        cli.read_source,
        cli.jobs.clamp(1, 16),
        if cli.read_source {
            "source-bytes"
        } else {
            "one-repeated-8mib-random-block"
        },
    );
    let mut totals: Vec<Vec<f64>> = vec![Vec::new(); kinds.len()];
    for rep in 0..cli.reps {
        for step in 0..kinds.len() {
            let index = (rep + step) % kinds.len();
            let Some(kind) = kinds.get(index).copied() else {
                continue;
            };
            drain(&dir)?;
            let resident = corpus_residency(&source, &shape.0)?;
            let root = dir.join(format!(
                ".bulkload-micro-corpus-{}-{rep}-{step}",
                std::process::id()
            ));
            let measured = corpus_sample(cli, &source, &shape, &root, kind);
            let removed = fs::remove_dir_all(&root);
            let sample = measured?;
            removed?;
            let total = millis(sample.total);
            if let Some(list) = totals.get_mut(index) {
                list.push(total);
            }
            println!(
                "micro name=durable-corpus variant={} rep={rep} order={step} files={} bytes={bytes} read_source={} source_resident_fraction_before={resident:.4} read_ms={:.3} write_ms={:.3} file_flush_ms={:.3} dir_flush_ms={:.3} total_ms={total:.3}",
                kind.label(),
                shape.0.len(),
                cli.read_source,
                millis(sample.times.read),
                millis(sample.times.write),
                millis(sample.times.file_flush),
                millis(sample.dir_flush),
            );
        }
    }
    for (kind, list) in kinds.iter().zip(totals.iter_mut()) {
        let (min, middle, max) = summary(list);
        println!(
            "micro_summary name=durable-corpus variant={} reps={} read_source={} min_ms={min:.3} median_ms={middle:.3} max_ms={max:.3}",
            kind.label(),
            list.len(),
            cli.read_source,
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
    fn residency_counts_pages_of_a_file() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("resident");
        fs::write(&path, random_bytes(3 * 16384 + 1, 3))?;
        let (resident, pages) = residency(&path)?;
        assert!(pages >= 1);
        assert!(resident <= pages);
        fs::write(dir.path().join("empty"), b"")?;
        assert_eq!(residency(&dir.path().join("empty"))?, (0, 0));
        Ok(())
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn durable_corpus_mirrors_shape_and_leaves_no_scratch() -> io::Result<()> {
        let source = tempfile::tempdir()?;
        fs::create_dir(source.path().join("nested"))?;
        fs::write(source.path().join("a"), random_bytes(70_000, 1))?;
        fs::write(source.path().join("nested/b"), random_bytes(1_000, 2))?;
        fs::write(source.path().join("nested/empty"), b"")?;
        let (files, directories) = corpus_shape(source.path())?;
        assert_eq!(files.len(), 3);
        assert_eq!(directories, vec![PathBuf::from("nested")]);
        let target = tempfile::tempdir()?;
        let dir = target
            .path()
            .to_str()
            .ok_or_else(|| io::Error::other("path"))?;
        let src = source
            .path()
            .to_str()
            .ok_or_else(|| io::Error::other("path"))?;
        for extra in [&[][..], &["--read-source", "--jobs", "2"][..]] {
            let mut words = vec![
                "micro",
                "durable-corpus",
                "--dir",
                dir,
                "--source",
                src,
                "--reps",
                "2",
            ];
            words.extend_from_slice(extra);
            run(&args(&words))?;
        }
        assert_eq!(fs::read_dir(target.path())?.count(), 0);
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
