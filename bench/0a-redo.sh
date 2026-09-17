#!/usr/bin/env bash
# 实验 0a-redo：带负载守卫的设备微基准。
#
# 行为：等待 CPU 空闲窗口（连续采样 idle ≥ 85%），然后运行 fio 套件，
# 全程每秒采样 /proc/loadavg 与 /proc/stat，结束后依据采样数据把本轮
# 判定为 clean 或 contaminated。判定、采样日志与 fio JSON 全部落盘。
#
# 退出码：0 = 完成且有判定；42 = 等待超时未获空闲窗口。
set -u

BENCH_DIR=/home/z/hf/flashdb-bench
EV=/home/z/vibe/flashdb/.scratch/05-engine-v0/evidence/0a-redo
IDLE_NEED=85        # 要求的 cpu idle %
CONSEC=3            # 需连续命中的采样数
SAMPLE_SEC=5
WAIT_LIMIT=7200     # 最长等待 2h，超时 exit 42
RUN_SEC=15
LOAD_CEIL=4.0       # 运行期间 load1 超过此值 → 标记 interference

mkdir -p "$EV"

read_cpu() {
    # → "busy idle"（jiffies）
    read -r _ u n s i w q sq st _ < /proc/stat
    echo $((u + n + s + q + sq + st)) $((i + w))
}

log() { echo "$(date -Is) $*" | tee -a "$EV/run.log"; }

# ---------- 1. 等待空闲窗口 ----------
log "waiting for idle window (idle>=${IDLE_NEED}% x${CONSEC}, sample=${SAMPLE_SEC}s, limit=${WAIT_LIMIT}s)"
prev=$(read_cpu)
consec=0
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
    if [ "$idle" -ge "$IDLE_NEED" ]; then
        consec=$((consec + 1))
        [ "$consec" -ge "$CONSEC" ] && { log "idle window acquired"; break; }
    else
        consec=0
    fi
    [ $(( $(date +%s) - t0 )) -ge "$WAIT_LIMIT" ] && {
        log "GIVE UP: no idle window within ${WAIT_LIMIT}s"; exit 42; }
done

# ---------- 2. 启动全程采样器 ----------
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

cleanup() { kill "$SAMPLER_PID" 2>/dev/null || true; }
trap cleanup EXIT

# ---------- 3. 预分配测试文件 ----------
if [ ! -f "$BENCH_DIR/f.8g" ]; then
    mkdir -p "$BENCH_DIR"
    fio --name=prep --filename="$BENCH_DIR/f.8g" --size=8G --rw=write \
        --bs=1M --direct=1 >/dev/null 2>&1
fi

# ---------- 4. fio 套件 ----------
run_fio() { # name rw bs qd engine
    fio --name="$1" --filename="$BENCH_DIR/f.8g" --rw="$2" --bs="$3" \
        --iodepth="$4" --ioengine="$5" --direct=1 --runtime="$RUN_SEC" \
        --time_based=1 --size=8G --output-format=json \
        --output="$EV/fio-$1.json" >/dev/null 2>&1
}
run_fio 4k-qd1  randread 4096  1  psync
run_fio 4k-qd8  randread 4096  8  libaio
run_fio 4k-qd32 randread 4096  32 libaio
run_fio 4k-qd64 randread 4096  64 libaio
run_fio 16k-qd1 randread 16384 1  psync
run_fio 16k-qd32 randread 16384 32 libaio
run_fio 64k-qd1 randread 65536 1  psync
run_fio seq    read     1048576 8  libaio

# ---------- 5. 判定 ----------
kill "$SAMPLER_PID" 2>/dev/null; wait "$SAMPLER_PID" 2>/dev/null || true
max_load=$(awk -F, 'NR>1 && $2+0>m {m=$2+0} END{print m}' "$SAMPLER")
verdict=clean
awk -F, 'NR>1 && $2+0>4.0 {bad=1} END{exit !bad}' "$SAMPLER" && verdict=interference_detected

python3 - "$EV" "$verdict" "$max_load" <<'PY'
import json, sys, os
ev, verdict, max_load = sys.argv[1], sys.argv[2], float(sys.argv[3])
def lat(name):
    j = json.load(open(f"{ev}/fio-{name}.json"))["jobs"][0]["read"]
    p = j["clat_ns"]["percentile"]
    return {"iops": round(j["iops"], 1),
            "mean_us": round(j["clat_ns"]["mean"]/1000, 1),
            "p99_us": round(p.get("99.000000", 0)/1000, 1)}
out = {
  "experiment": "0a-redo",
  "verdict": verdict,
  "max_load1_during_run": max_load,
  "device": "Colorful CN600 2TB (/home/z/hf, no encryption)",
  "results": {n: lat(f"4k-{n}") for n in ["qd1", "qd8", "qd32", "qd64"]},
  "results_16k": {"qd1": lat("16k-qd1"), "qd32": lat("16k-qd32")},
  "results_64k": {"qd1": lat("64k-qd1")},
  "seq_bw_MiB_s": round(json.load(open(f"{ev}/fio-seq.json"))["jobs"][0]["read"]["bw_bytes"]/2**20, 1),
}
json.dump(out, open(f"{ev}/summary.json", "w"), indent=1)
print(json.dumps(out, indent=1))
PY
log "verdict=$verdict max_load1=$max_load — done"
[ "$verdict" = "clean" ]
