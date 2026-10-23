#!/usr/bin/env python3
# examples/mahm-reader.py — reads /dev/shm/MAHMSharedMemory published by
# afterburner-rs with the byte layout defined by the interop headers
# (MAX_PATH=260, entrySize=1324). Any third-party Windows tool that talks
# to the Windows object with this layout will read equivalent data here.

import struct

MAHM = "/dev/shm/MAHMSharedMemory"
MAX_PATH = 260

def main():
    b = open(MAHM, "rb").read()
    sig, ver, hdr, n, esz, t, ng, gesz = struct.unpack_from("<IIIIIIII", b, 0)
    sig_txt = chr(sig & 0xFF) + chr((sig >> 8) & 0xFF) + chr((sig >> 16) & 0xFF) + chr((sig >> 24) & 0xFF)
    print(f"signature={sig_txt} version={ver:#x} entries={n} entrySize={esz} gpus={ng}")
    for i in range(n):
        o = hdr + i * esz
        (name,) = struct.unpack_from(f"{MAX_PATH}s", b, o)
        (units,) = struct.unpack_from(f"{MAX_PATH}s", b, o + MAX_PATH)
        data, mnl, mxl, flags, gpu, sid = struct.unpack_from("<fffIII", b, o + 5 * MAX_PATH)
        name = name.split(b"\0")[0].decode()
        units = units.split(b"\0")[0].decode()
        shown = "-" if data >= 3.4e38 else f"{data:.1f}"
        print(f"  {name:<24} {shown:>10} {units:<6} gpu={gpu} id=0x{sid:02x}")
    for i in range(ng):
        o = hdr + n * esz + i * gesz
        (gid,) = struct.unpack_from(f"{MAX_PATH}s", b, o)
        (dev,) = struct.unpack_from(f"{MAX_PATH}s", b, o + MAX_PATH * 2)
        gid = gid.split(b"\0")[0].decode()
        dev = dev.split(b"\0")[0].decode()
        print(f"  GPU{i}: {gid}  {dev}")

if __name__ == "__main__":
    main()
