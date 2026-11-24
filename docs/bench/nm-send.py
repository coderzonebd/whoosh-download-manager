# SPDX-License-Identifier: GPL-3.0-or-later
"""Talk to wdm-nmhost as the browser extension does (native messaging: a
4-byte little-endian length, then JSON), for docs/bench/windows-handcheck.ps1.

    python nm-send.py <wdm-nmhost.exe> <seconds to stay> <json message>...

Each message is sent in turn; every reply and event is printed as one line.
The host is kept open for <seconds to stay>, as a browser keeps it while the
extension is loaded, so the windows it asked for have time to appear.
"""
import json
import struct
import subprocess
import sys
import threading
import time


def main() -> int:
    host, stay, messages = sys.argv[1], float(sys.argv[2]), sys.argv[3:]
    p = subprocess.Popen(
        [host, "chrome-extension://handcheck/"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
    )

    def read() -> None:
        while True:
            head = p.stdout.read(4)
            if len(head) < 4:
                return
            (n,) = struct.unpack("<I", head)
            print("<", p.stdout.read(n).decode("utf-8", "replace"), flush=True)

    threading.Thread(target=read, daemon=True).start()
    for m in messages:
        data = json.dumps(json.loads(m)).encode()
        p.stdin.write(struct.pack("<I", len(data)) + data)
        p.stdin.flush()
        print(">", m, flush=True)
        time.sleep(2)
    time.sleep(stay)
    p.stdin.close()
    try:
        p.wait(5)
    except subprocess.TimeoutExpired:
        p.kill()
    return 0


if __name__ == "__main__":
    sys.exit(main())
