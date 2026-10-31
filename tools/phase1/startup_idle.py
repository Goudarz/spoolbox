"""Measure time from process spawn to first HTTP 200 on /readyz, and idle memory.

Usage: python -I startup_idle.py <path-to-spoolbox> [runs]
Memory is read with PowerShell on Windows (WorkingSet64 / PrivateMemorySize64) and /proc on Linux.
"""
import json, os, socket, statistics, subprocess, sys, time

BIN = sys.argv[1]
RUNS = int(sys.argv[2]) if len(sys.argv) > 2 else 20
PORT = 28925


def ready():
    try:
        with socket.create_connection(("127.0.0.1", PORT), timeout=0.005) as s:
            s.sendall(b"GET /readyz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            return s.recv(16).startswith(b"HTTP/1.1 200")
    except OSError:
        return False


def spawn():
    env = {k: v for k, v in os.environ.items() if not k.startswith(("MP_", "SPOOLBOX_"))}
    return subprocess.Popen([BIN, "--listen", f"127.0.0.1:{PORT}", "--smtp", "127.0.0.1:0", "-q"],
                            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def memory(pid):
    if os.name == "nt":
        out = subprocess.run(["powershell", "-NoProfile", "-Command",
                              f"$p=Get-Process -Id {pid}; \"$($p.WorkingSet64) $($p.PrivateMemorySize64)\""],
                             capture_output=True, text=True).stdout.split()
        return {"working_set_bytes": int(out[0]), "private_bytes": int(out[1])}
    status = open(f"/proc/{pid}/status").read()
    rss = next(int(l.split()[1]) for l in status.splitlines() if l.startswith("VmRSS"))
    return {"rss_bytes": rss * 1024}


times = []
for _ in range(RUNS):
    t = time.perf_counter()
    p = spawn()
    while not ready():
        if p.poll() is not None:
            sys.exit("server exited")
    times.append((time.perf_counter() - t) * 1000)
    p.kill(); p.wait()

p = spawn()
while not ready():
    time.sleep(0.01)
time.sleep(10)
mem = memory(p.pid)
p.kill(); p.wait()

print(json.dumps({"binary_bytes": os.path.getsize(BIN), "startup_to_ready_ms": {
    "runs": [round(x, 1) for x in times], "median": round(statistics.median(times), 1), "max": round(max(times), 1)},
    "idle_after_10s": mem}, indent=2))
