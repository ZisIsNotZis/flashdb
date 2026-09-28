//! 实验 0a-redo：O_DIRECT 设备微基准（纯 std，无依赖）。
//!
//! 测量引擎将使用的真实访问模式：O_DIRECT `read_at` 页读。
//! 用法：`nvme-bench <file> <mode>`，mode ∈ qd1|qd8|qd32|qd64（4K）|seq（1M）。
//!
//! 偏移与缓冲区 4K 对齐（O_DIRECT 要求）；随机偏移由 SplitMix64 生成。
//! 每线程独立缓冲区与独立种子；结果合并算 IOPS / 均值 / p99 / max。

use std::alloc::{alloc, dealloc, Layout};
use std::fs::File;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::time::Instant;

const O_DIRECT: i32 = 0o4_0000; // x86_64 linux
const ALIGN: usize = 4096;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

struct AlignedBuf {
    ptr: *mut u8,
    layout: Layout,
}

impl AlignedBuf {
    fn new(size: usize) -> AlignedBuf {
        let layout = Layout::from_size_align(size, ALIGN).unwrap();
        let ptr = unsafe { alloc(layout) };
        assert!(!ptr.is_null());
        AlignedBuf { ptr, layout }
    }
    fn as_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.layout.size()) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, self.layout) };
    }
}

fn open_direct(path: &str) -> File {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_DIRECT)
        .open(path)
        .expect("open O_DIRECT")
}

fn p99(xs: &mut [u128]) -> u128 {
    xs.sort_unstable();
    xs[(xs.len() as f64 * 0.99) as usize % xs.len().max(1)]
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: nvme-bench <file> <qd1|qd8|qd32|qd64|seq>");
        std::process::exit(2);
    }
    let (file, mode) = (args[1].clone(), args[2].clone());
    let f = open_direct(&file);
    let fsize = f.metadata().unwrap().len();
    let n_pages = fsize / ALIGN as u64;
    let mut seed_rng = Rng(0x5EED_BEEF);
    let load1: f64 = std::fs::read_to_string("/proc/loadavg")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    println!("# file={file} size={fsize} mode={mode} load1={load1}");

    match mode.as_str() {
        // 顺序 1M 大块读：带宽
        "seq" => {
            let bs = 1 << 20;
            let mut buf = AlignedBuf::new(bs);
            let mut total = 0u64;
            let mut off = 0u64;
            let t = Instant::now();
            while t.elapsed().as_secs() < 10 {
                let n = bs.min((fsize - off) as usize);
                FileExt::read_at(&f, &mut buf.as_mut()[..n], off).unwrap();
                off = (off + n as u64) % fsize;
                total += n as u64;
            }
            let el = t.elapsed().as_secs_f64();
            println!("seq: bw = {:.1} MiB/s", total as f64 / 2f64.powi(20) / el);
        }
        // 随机 4K 读：qd 后缀 = 线程数（并发深度）
        m if m.starts_with("qd") => {
            let depth: usize = m[2..].parse().unwrap();
            let n_per_thread = 3000;
            let seeds: Vec<u64> = (0..depth).map(|_| seed_rng.next()).collect();
            let t = Instant::now();
            let lat_all = std::thread::scope(|s| {
                let fref = &f;
                let handles: Vec<_> = seeds
                    .into_iter()
                    .map(|seed| {
                        s.spawn(move || {
                            let mut rng = Rng(seed);
                            let mut buf = AlignedBuf::new(4096);
                            let mut lat = Vec::with_capacity(n_per_thread);
                            for _ in 0..n_per_thread {
                                let off = rng.below(n_pages) * ALIGN as u64;
                                let t0 = Instant::now();
                                FileExt::read_at(fref, &mut buf.as_mut()[..4096], off).unwrap();
                                lat.push(t0.elapsed().as_micros());
                            }
                            lat
                        })
                    })
                    .collect::<Vec<_>>();
                let mut all = Vec::new();
                for h in handles {
                    all.extend(h.join().unwrap());
                }
                all
            });
            let el = t.elapsed().as_secs_f64();
            let n = lat_all.len();
            let sum: u128 = lat_all.iter().sum();
            let worst = *lat_all.iter().max().unwrap();
            let p99 = p99(&mut lat_all.clone());
            let mean = sum as f64 / n as f64;
            let iops = n as f64 / el;
            println!("{mode}: iops={iops:.0} mean={mean:.1}us p99={p99}us max={worst}us (n={n})");
        }
        other => {
            eprintln!("unknown mode {other}");
            std::process::exit(2);
        }
    }
}
