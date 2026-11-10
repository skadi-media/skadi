#!/usr/bin/env python3
"""Relay a local TCP port to a TV's developer port (26101).

`sdb connect <tv>` can fail on macOS with "failed to connect to remote
target" although the TV answers: the sdb server process is not allowed to
reach the local network. A process started from a terminal is, so this relays
for it:

    tools/sdb-relay.py <tv-address> 36101 &
    sdb connect 127.0.0.1:36101

Pick a local port outside 26101-26199, or sdb lists the TV as an emulator.
"""
import socket
import sys
import threading


def pump(src, dst):
    try:
        while True:
            data = src.recv(65536)
            if not data:
                break
            dst.sendall(data)
    except OSError:
        pass
    for s in (src, dst):
        try:
            s.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    tv = (sys.argv[1], 26101)
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", int(sys.argv[2])))
    listener.listen(8)
    while True:
        client, _ = listener.accept()
        try:
            remote = socket.create_connection(tv, 5)
        except OSError as e:
            sys.stderr.write("cannot reach the TV: %s\n" % e)
            client.close()
            continue
        # The connect timeout must not become a read timeout: an idle sdb
        # session is normal.
        remote.settimeout(None)
        threading.Thread(target=pump, args=(client, remote), daemon=True).start()
        threading.Thread(target=pump, args=(remote, client), daemon=True).start()


if __name__ == "__main__":
    main()
