#!/usr/bin/env bash
# 实验 0a-redo：分级负载守卫的设备微基准。
#
# 两级窗口：
#   clean        — 连续 3 采样 idle ≥ 85%（理想条件，可作校准输入）
#   reduced-load — 连续 4 采样 idle ≥ 50%（次优条件，结果标注为参考值）
#
# 运行期间每秒采样 loadavg/idle 写入 CSV；结束后按运行期最大 load1 与
# 等级综合判定 clean / reduced-load / interference_detected。
#
# 退出码：0 = 完成且有判定；42 = 等待超时未获任何窗口。
set -eu

BENCH_DIR=/home/z/hf/flashdb-bench
EV=/home/z/vibe/flashdb/.scratch/05-engine-v0/evidence/0a-redo
STRICT=85
FALLBACK=50
CONSEC=3
FB_CONSEC=4
SAMPLE_SEC=5
WAIT_LIMIT=${WAIT_LIMIT:-86400}   # 默认 24h，可用环境变量覆盖
RUN_SEC=15
LOAD_CEIL=4.0

mkdir -p "$EV"

read_cpu() {
    read -r _ u n s i w q sq st _ < /proc/stat
    echo $((u + n + s + q + sq + st)) $((i + w))
}

log() { echo "$(date -Is) $*" | tee -a "$EV/run.log"; }

# ---------- 1. 等待窗口（分级） ----------
log "waiting: strict(>=${STRICT}% x${CONSEC}) preferred, fallback(>=${FALLBACK}% x${FB_CONSEC}), limit=${WAIT_LIMIT}s"
prev=$(read_cpu)
strict_n=0; fb_n=0; mode=""
t0=$(date +%s)
while :; do
    sleep "$SAMPLE_SEC"
    cur=$(read_cpu)
    set -- $prev; pb=$1; pi=$2
    set -- $cur;  cb=$1; ci=$2
    dt=$(( (cb + ci) - (pb + pi) ))
    prev=$cur
    [ "$dt" -le 0 ] && continue
    idle=$(( (ci - pi) * 100 / dt ))
    log "wait: idle=${idle}%"
    if [ "$idle" -ge "$STRICT" ]; then
        strict_n=$((strict_n + 1)); fb_n=$((fb_n + 1))
        if [ "$strict_n" -ge "$CONSEC" ]; then mode="clean"; log "strict window acquired"; break; fi
    elif [ "$idle" -ge "$FALLBACK" ]; then
        fb_n=$((fb_n + 1)); strict_n=0
        if [ "$fb_n" -ge "$FB_CONSEC" ]; then mode="reduced-load"; log "fallback window acquired"; break; fi
    else
        strict_n=0; fb_n=0
    fi
    [ $(( $(date +%s) - t0 )) -ge "$WAIT_LIMIT" ] && {
        log "GIVE UP: no window within ${WAIT_LIMIT}s"; exit 42; }
done
[ -z "$mode" ] && { log "no window"; exit 42; }

# ---------- 2. 全程采样器 ----------
SAMPLER="$EV/loadlog.csv"
echo "ts,load1,idle_pct" > "$SAMPLER"
( while :; do
    l=$(cut -d' ' -f1 /proc/loadavg)
    p=$(read_cpu); sleep 1; c=$(read_cpu)
    set -- $p; pb=$1; pi=$2
    set -- $c;  cb=$1; ci=$2
    dt=$(( (cb + ci) - (pb + pi) ))
    idle=0; [ "$dt" -gt 0 ] && idle=$(( (ci - pi) * 100 / dt ))
    echo "$(date -Is),$l,$idle" >> "$SAMPLER"
  done ) &
SAMPLER_PID=$!
trap 'kill "$SAMPLER_PID" 2>/dev/null || true' EXIT

# ---------- 3. 测试文件 ----------
command -v fio >/dev/null 2>&1 || { log "fio missing — cannot run full calibration"; exit 127; }
if [ ! -f "$BENCH_DIR/f.8g" ]; then
    mkdir -p "$BENCH_DIR"
    fio --name=prep --filename="$BENCH_DIR/f.8g" --size=8G --rw=write \
        --bs=1M --direct=1 >/dev/null 2>&1
fi

# ---------- 4. fio 套件 ----------
rm -f "$EV"/fio-*.json "$EV"/summary.json
run_fio() { # name rw bs qd engine
    fio --name="$1" --filename="$BENCH_DIR/f.8g" --rw="$2" --bs="$3" \
        --iodepth="$4" --ioengine="$5" --direct=1 --runtime="$RUN_SEC" \
        --time_based=1 --size=8G --output-format=json \
        --output="$EV/fio-$1.json" >/dev/null 2>&1 || { log "fio $1 failed"; exit 1; }
    [ -s "$EV/fio-$1.json" ] || { log "fio $1 produced no JSON"; exit 1; }
}
run_fio 4k-qd1  randread 4096  1  psync
run_fio 4k-qd8  randread 4096  8  libaio
run_fio 4k-qd32 randread 4096  32 libaio
run_fio 4k-qd64 randread 4096  64 libaio
run_fio 16k-qd1 randread 16384 1  psync
run_fio 16k-qd32 randread 16384 32 libaio
run_fio 64k-qd1 randread 65536 1  psync
run_fio seq    read     1048576 8  libaio

# ---------- Rust 基准（同一空闲窗口内，测量引擎真实访问模式：O_DIRECT pread 页读） ----------
NB=/home/z/vibe/flashdb/target/release/nvme-bench
if [ -x "$NB" ]; then
    for m in qd1 qd8 qd32 qd64; do "$NB" "$BENCH_DIR/f.8g" "$m" > "$EV/nvme-$m.txt" 2>&1; done
    "$NB" "$BENCH_DIR/f.8g" seq > "$EV/nvme-seq.txt" 2>&1
else
    log "nvme-bench binary missing at $NB — skipped"
fi

# ---------- 5. 判定 ----------
kill "$SAMPLER_PID" 2>/dev/null; wait "$SAMPLER_PID" 2>/dev/null || true
max_load=$(awk -F, 'NR>1 && $2+0>m {m=$2+0} END{print m}' "$SAMPLER")
min_idle=$(awk -F, 'NR>1 && NF==3 {if (!seen || $3+0<mi) mi=$3+0; seen=1} END{if (!seen) exit 1; print mi}' "$SAMPLER")
verdict="$mode"
# 按整个运行期最低瞬时 idle 重新评级，不能仅凭进入窗口时的等级报 clean。
# load1 是滞后指标，会把渲染的 D 态线程计入（见 three-run-analysis.md）。
if [ "$min_idle" -lt 50 ]; then verdict="interference_detected"
elif [ "$min_idle" -lt 85 ]; then verdict="reduced-load"
fi

python3 - "$EV" "$verdict" "$max_load" "$mode" <<'PY'
import json, sys
ev, verdict, max_load, mode = sys.argv[1], sys.argv[2], float(sys.argv[3]), sys.argv[4]
def lat(name):
    j = json.load(open(f"{ev}/fio-{name}.json"))["jobs"][0]["read"]
    p = j["clat_ns"]["percentile"]
    return {"iops": round(j["iops"], 1),
            "mean_us": round(j["clat_ns"]["mean"]/1000, 1),
            "p99_us": round(p.get("99.000000", 0)/1000, 1)}
out = {
  "experiment": "0a-redo",
  "window_mode": mode,
  "verdict": verdict,
  "max_load1_during_run": max_load,
  "note": ("ideal window; usable as calibration input" if verdict == "clean"
           else "reduced-load run; indicative constants, prefer an idle re-run for final calibration"
           if verdict == "reduced-load" else "interference detected; discard measurements"),
  "device": "Colorful CN600 2TB (/home/z/hf, no encryption)",
  "results": {n: lat(f"4k-{n}") for n in ["qd1", "qd8", "qd32", "qd64"]},
  "results_16k": {"qd1": lat("16k-qd1"), "qd32": lat("16k-qd32")},
  "results_64k": {"qd1": lat("64k-qd1")},
  "seq_bw_MiB_s": round(json.load(open(f"{ev}/fio-seq.json"))["jobs"][0]["read"]["bw_bytes"]/2**20, 1),
}
json.dump(out, open(f"{ev}/summary.json", "w"), indent=1)
print(json.dumps(out, indent=1))
PY
log "mode=$mode verdict=$verdict max_load1=$max_load min_idle=$min_idle — done"
[ "$verdict" != "interference_detected" ]
