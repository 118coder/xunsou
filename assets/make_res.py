# 生成 assets/app.res（32 位 .RES 规范，含 RT_ICON×7 + RT_GROUP_ICON）。
#
# 本机没有 Windows SDK 的 rc.exe，所以资源由本脚本直接写出。
# 注意：.RES 里**不能**有首尾的空资源条目——MSVC 的 cvtres.exe 会判为损坏。
#
# 用法：
#   python assets/make_res.py                       # 依赖 assets/app.ico
#   cvtres /MACHINE:X64 /OUT:assets/app.obj assets/app.res
import struct

data = open('assets/app.ico', 'rb').read()
_, _, count = struct.unpack('<HHH', data[:6])
entries = []
off = 6
for _ in range(count):
    w, h, _, _, planes, bits, size, offset = struct.unpack('<BBBBHHII', data[off:off + 16])
    off += 16
    entries.append((w or 256, size, data[offset:offset + size]))
entries.sort()


def entry(rtype, rid, payload):
    # Type / Name 均为序数形式（0xFFFF + WORD）；
    # DataVersion=0、MemoryFlags=0x1030（MOVEABLE|PURE|DISCARDABLE）、Lang=0x0409
    hdr = struct.pack('<HH', 0xFFFF, rtype) + struct.pack('<HH', 0xFFFF, rid)
    hdr += struct.pack('<I', 0) + struct.pack('<H', 0x1030) + struct.pack('<H', 0x0409)
    hdr += struct.pack('<I', 0) + struct.pack('<I', 0)
    rec = struct.pack('<II', len(payload), len(hdr)) + hdr
    rec += b'\x00' * ((4 - len(rec) % 4) % 4)
    return rec + payload + b'\x00' * ((4 - len(payload) % 4) % 4)


res = b''
gi = b''
for i, (w, size, png) in enumerate(entries, start=1):
    res += entry(3, i, png)  # RT_ICON，ID = 1..n
    gi += struct.pack('<BBBBHHIH', w % 256, w % 256, 0, 0, 1, 32, size, i)
res += entry(14, 1, struct.pack('<HHH', 0, 1, len(entries)) + gi)  # RT_GROUP_ICON
open('assets/app.res', 'wb').write(res)
print('app.res:', len(res), 'bytes,', len(entries), 'icons')
