"""Reject accidental dynamic CRT dependencies in the redistributable runtime."""
import pathlib
import struct
import sys

data = pathlib.Path(sys.argv[1]).read_bytes()
pe = struct.unpack_from("<I", data, 60)[0]
assert data[pe:pe + 4] == b"PE\0\0"
optional = pe + 24
assert struct.unpack_from("<H", data, optional)[0] == 0x20B
count = struct.unpack_from("<H", data, pe + 6)[0]
size = struct.unpack_from("<H", data, pe + 20)[0]
sections = [struct.unpack_from("<IIII", data, optional + size + i * 40 + 8) for i in range(count)]


def offset(rva):
    for virtual_size, address, raw_size, position in sections:
        if address <= rva < address + max(virtual_size, raw_size):
            return position + rva - address
    raise ValueError("Import RVA is outside the PE sections")


position = offset(struct.unpack_from("<I", data, optional + 120)[0])
imports = []
while any(data[position:position + 20]):
    name = offset(struct.unpack_from("<I", data, position + 12)[0])
    imports.append(data[name:data.index(b"\0", name)].decode("ascii"))
    position += 20
for name in imports:
    assert not name.lower().startswith(("vcruntime", "msvcp", "api-ms-win-crt-")), name
print("Windows runtime uses the static CRT")
