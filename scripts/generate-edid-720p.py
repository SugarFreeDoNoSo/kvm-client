#!/usr/bin/env python3
"""Generate HDMI EDID with preferred 720p60 plus VGA fallback; not an FPS cap."""
from pathlib import Path


def descriptor(tag: int, text: str) -> bytes:
    payload = text.encode("ascii") + b"\n"
    if len(payload) > 13:
        raise ValueError("EDID descriptor text exceeds 13 bytes")
    return bytes([0, 0, 0, tag, 0]) + payload.ljust(13, b" ")


def checksum(block: bytearray) -> bytes:
    if len(block) != 128:
        raise ValueError("EDID blocks must be 128 bytes")
    block[-1] = (-sum(block[:-1])) & 0xFF
    return bytes(block)


def make_edid() -> bytes:
    base = bytearray(128)
    base[:8] = bytes.fromhex("00ffffffffffff00")
    manufacturer = (11 << 10) | (22 << 5) | 13  # KVM
    base[8:10] = manufacturer.to_bytes(2, "big")
    base[10:12] = (720).to_bytes(2, "little")
    base[12:16] = (1).to_bytes(4, "little")
    # HDMI's vendor-specific block requires EDID 1.3. EDID 1.3 does not encode
    # interface/depth in the digital-input byte; those bits remain reserved.
    base[16:20] = bytes([0, 2026 - 1990, 1, 3])
    base[20:25] = bytes([0x80, 32, 18, 120, 0x0E])  # Digital RGB, sRGB, preferred DTD
    coordinates = [655, 338, 307, 614, 154, 61, 320, 337]
    base[25] = sum((value & 3) << (6 - i * 2) for i, value in enumerate(coordinates[:4]))
    base[26] = sum((value & 3) << (6 - i * 2) for i, value in enumerate(coordinates[4:]))
    base[27:35] = bytes(value >> 2 for value in coordinates)
    # CTA requires VGA support as an established timing or VIC 1. This is a
    # lower-resolution recovery mode, not an additional high-resolution mode.
    base[35:38] = bytes([0x20, 0, 0])  # 640x480p60 (59.94 Hz)
    base[38:54] = bytes([1, 1]) * 8
    # CTA-861 720p60: 74.25 MHz, totals 1650x750, +HSync/+VSync.
    # The physical size agrees with the 32x18cm basic-display parameters.
    base[54:72] = bytes.fromhex("011d007251d01e206e28550040b41000001e")
    base[72:90] = descriptor(0xFC, "KVM 720p60")
    base[90:108] = descriptor(0xFF, "KVM720P001")
    # EDID 1.3 uses the default-GTF range descriptor tag, not 1.4's range-only
    # tag. The base feature byte does not enable the optional default-GTF bit.
    # Bounds include VGA 59.94Hz/31.469kHz and 720p60 60Hz/45kHz.
    base[108:126] = bytes([0, 0, 0, 0xFD, 0, 59, 60, 31, 45, 8, 0, 0x0A]) + b" " * 6
    base[126] = 1

    cta = bytearray(128)
    cta[:4] = bytes([0x02, 0x03, 16, 0x80])  # Underscanned video; no audio.
    # Native VIC4, fallback VIC1, HDMI OUI 000c03/address 1.0.0.0, and VCDB.
    # VCDB: selectable RGB quantization; PT/IT/CE formats all underscanned.
    cta[4:16] = bytes([
        0x42, 0x84, 0x01,
        0x65, 0x03, 0x0C, 0x00, 0x10, 0x00,
        0xE2, 0x00, 0x6A,
    ])
    return checksum(base) + checksum(cta)


def validate(edid: bytes) -> None:
    assert len(edid) == 256 and edid[:8] == bytes.fromhex("00ffffffffffff00")
    assert edid[126] == 1
    assert edid[18:21] == bytes([1, 3, 0x80])
    assert all(sum(edid[offset:offset + 128]) % 256 == 0 for offset in [0, 128])
    timing = edid[54:72]
    clock = int.from_bytes(timing[:2], "little") * 10_000
    width = timing[2] | ((timing[4] >> 4) << 8)
    hblank = timing[3] | ((timing[4] & 15) << 8)
    height = timing[5] | ((timing[7] >> 4) << 8)
    vblank = timing[6] | ((timing[7] & 15) << 8)
    assert (width, height) == (1280, 720)
    assert clock / ((width + hblank) * (height + vblank)) == 60
    assert timing[12] | ((timing[14] >> 4) << 8) == 320
    assert timing[13] | ((timing[14] & 15) << 8) == 180
    assert edid[35:38] == bytes([0x20, 0, 0])
    assert edid[132:135] == bytes([0x42, 0x84, 0x01])
    assert edid[141:144] == bytes([0xE2, 0x00, 0x6A])


if __name__ == "__main__":
    edid = make_edid()
    validate(edid)
    destination = Path(__file__).resolve().parents[1] / "profiles" / "edid"
    destination.mkdir(parents=True, exist_ok=True)
    (destination / "kvm-720p60.bin").write_bytes(edid)
    (destination / "kvm-720p60.hex").write_text(edid.hex() + "\n", encoding="ascii")
    print("Generated 256-byte HDMI EDID 1.3: preferred 1280x720p60 + VGA fallback; encoder FPS unchanged.")
