"""Show a board's crash records with source lines: fw-crash [host] (default: max).

The password comes from $MAX_PASSWORD or esp/secrets.toml. The backtrace is
resolved against the ELF in target/ of this checkout, which must be the build the board
ran (see the version), using addr2line for the board's chip.
"""
import base64, json, os, re, shutil, subprocess, sys, urllib.request
from datetime import datetime
from pathlib import Path

here = Path(__file__).resolve().parent.parent
host = sys.argv[1] if len(sys.argv) > 1 else "max"
password = os.environ.get("MAX_PASSWORD")
if not password:
    m = re.search(r'^MAX_PASSWORD\s*=\s*"([^"]*)"', (here / "secrets.toml").read_text(errors="ignore"), re.M) if (here / "secrets.toml").exists() else None
    password = m and m.group(1)
if not password:
    sys.exit("Set MAX_PASSWORD (the device password)")
req = urllib.request.Request(f"http://{host}/api/crashes", headers={"Authorization": "Basic " + base64.b64encode(f"x:{password}".encode()).decode()})
crashes = json.load(urllib.request.urlopen(req, timeout=10))
if not crashes:
    print("No crashes recorded.")
for c in crashes:
    when = datetime.fromtimestamp(c["time"]).isoformat(" ") if c.get("time") else "time unknown"
    print(f"\n{when}  {c['board']}  max2mqtt {c['version']}  after {c['uptime_seconds']} s\n  {c['reason']}")
    if not c["backtrace"]:
        continue
    c3 = "C3" in c["board"]
    elf = here / "target" / ("riscv32imc-unknown-none-elf" if c3 else "xtensa-esp32-none-elf") / "release" / "max2mqtt"
    tool = shutil.which("addr2line" if c3 else "xtensa-esp32-elf-addr2line")
    if not (elf.exists() and tool):
        print("  " + " ".join(c["backtrace"]) + f"\n  (no {elf.name} build or addr2line here to resolve them)")
        continue
    out = subprocess.run([tool, "-pfiaC", "-e", str(elf), *c["backtrace"]], capture_output=True, text=True).stdout
    for line in out.splitlines():
        print("  " + line)
