"""Reading Debian packages without dpkg (setup-e2.py, e2-make-lock.py):
the files of a .deb's data archive, by path, as bytes. Python only, so
that E2 is built the same way on Linux and macOS."""
import io
import tarfile


def ar_members(data):
    """{name: bytes} of an ar archive (a .deb)."""
    if not data.startswith(b"!<arch>\n"):
        raise ValueError("not an ar archive")
    out, i = {}, 8
    while i + 60 <= len(data):
        head = data[i:i + 60]
        name = head[:16].decode().strip().rstrip("/")
        size = int(head[48:58].decode().strip())
        out[name] = data[i + 60:i + 60 + size]
        i += 60 + size + (size % 2)
    return out


def deb_members(data):
    """[(path without './', bytes)] of the regular files of a .deb."""
    parts = ar_members(data)
    name = next((n for n in parts if n.startswith("data.tar")), None)
    if name is None:
        raise ValueError("a .deb without data.tar")
    raw = parts[name]
    if name.endswith(".zst"):
        from compression import zstd  # Python 3.14
        raw = zstd.decompress(raw)
        mode = "r:"
    elif name.endswith(".xz"):
        mode = "r:xz"
    elif name.endswith(".gz"):
        mode = "r:gz"
    else:
        mode = "r:"
    out = []
    with tarfile.open(fileobj=io.BytesIO(raw), mode=mode) as t:
        for m in t.getmembers():
            if m.isfile():
                out.append((m.name[2:] if m.name.startswith("./") else m.name, t.extractfile(m).read()))
    return out
