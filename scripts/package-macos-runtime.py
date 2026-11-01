#!/usr/bin/env python3
"""Make an existing Tauri .app independent of Homebrew; sign it afterwards.

Usage: python3 scripts/package-macos-runtime.py 'target/release/bundle/macos/KVM Client.app'
Only installed, non-GPL GStreamer plugins needed by the receiver are selected.
The application/vendor licenses are separate from the runtime's licenses.
"""

import argparse
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import stat
import subprocess
import sys


PLUGINS = (
    "coreelements", "app", "webrtc", "dtls", "sctp", "rtp", "rtpmanager",
    "videoparsersbad", "opengl", "applemedia", "srtp", "udp",
    "opus", "audioconvert", "audioresample", "volume", "autodetect", "osxaudio",
)
FORBIDDEN = re.compile(r"(?:libgstlibav|libavcodec|libavformat|libavfilter|libavutil|libx264|libx265)", re.I)
SYSTEM_PREFIXES = ("/usr/lib/", "/System/Library/")


def command(*args):
    result = subprocess.run([str(arg) for arg in args], capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(f"Command failed: {' '.join(map(str, args))}\n{result.stderr.strip()}")
    return result.stdout


def version_tuple(value):
    return tuple(int(part) for part in value.split("."))


def macho(path):
    listing = command("/usr/bin/otool", "-L", path)
    deps = [line.strip().split(" (", 1)[0] for line in listing.splitlines()[1:] if line.startswith("\t")]
    ids = command("/usr/bin/otool", "-D", path).splitlines()[1:]
    identifier = next((line.strip() for line in ids if line.strip() and not line.endswith(":")), None)
    load = command("/usr/bin/otool", "-l", path)
    rpaths = re.findall(r"cmd LC_RPATH\s+cmdsize \d+\s+path (.+?) \(offset", load)
    minimums = re.findall(r"\bminos ([\d.]+)", load)
    minimums += re.findall(r"cmd LC_VERSION_MIN_MACOSX\s+cmdsize \d+\s+version ([\d.]+)", load)
    minimum = max(minimums, key=version_tuple) if minimums else "0.0"
    arches = set(command("/usr/bin/lipo", "-archs", path).split())
    return {"deps": [dep for dep in deps if dep != identifier], "id": identifier,
            "rpaths": rpaths, "minimum": minimum, "arches": arches}


def is_system(path):
    return str(path).startswith(SYSTEM_PREFIXES)


def expand_loader(value, source, executable):
    return value.replace("@loader_path", str(source.parent)).replace("@executable_path", str(executable.parent))


def resolve_dependency(dependency, source, executable, info, executable_info):
    if is_system(dependency):
        return None
    if dependency.startswith("@rpath/"):
        relative = dependency[len("@rpath/"):]
        candidates = [Path(expand_loader(rpath, source, executable)) / relative for rpath in info["rpaths"]]
        candidates += [Path(expand_loader(rpath, executable, executable)) / relative for rpath in executable_info["rpaths"]]
    else:
        candidates = [Path(expand_loader(dependency, source, executable))]
    for candidate in candidates:
        if candidate.is_absolute() and candidate.is_file():
            return candidate.resolve()
    raise RuntimeError(f"Cannot resolve {dependency} required by {source}")


def formula_for(path):
    parts = path.parts
    if "Cellar" not in parts:
        return None
    index = parts.index("Cellar")
    return parts[index + 1], parts[index + 2], Path(*parts[:index + 3])


def license_files(prefix):
    matches = []
    for entry in prefix.iterdir():
        if re.match(r"^(license|copying|notice|copyright|authors|lgpl|gpl|bsd|mit|apache)(?:[.\-_]|$)", entry.name, re.I):
            if entry.is_file():
                matches.append(entry)
            elif entry.is_dir():
                matches.extend(item for item in entry.rglob("*") if item.is_file())
    return sorted(matches)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path)
    parser.add_argument("--dry-run", action="store_true", help="Audit the complete closure without modifying the .app")
    args = parser.parse_args()
    if sys.platform != "darwin":
        raise RuntimeError("This packager requires macOS and Xcode command-line tools")
    app = args.app.resolve()
    contents = app / "Contents"
    plist = contents / "Info.plist"
    if app.suffix != ".app" or not plist.is_file():
        raise RuntimeError("Pass an existing Tauri .app containing Contents/Info.plist")
    metadata = plistlib.loads(plist.read_bytes())
    executable = contents / "MacOS" / metadata["CFBundleExecutable"]
    if not executable.is_file():
        raise RuntimeError(f"Missing application executable: {executable}")
    brew = shutil.which("brew")
    if not brew:
        brew = next((str(path) for path in [Path("/opt/homebrew/bin/brew"), Path("/usr/local/bin/brew")] if path.is_file()), None)
    if not brew:
        raise RuntimeError("Homebrew is required on the packaging machine")
    gst = Path(command(brew, "--prefix", "gstreamer").strip())
    nice = Path(command(brew, "--prefix", "libnice-gstreamer").strip())
    framework_dir = contents / "Frameworks"
    plugin_dir = contents / "Resources/gstreamer-1.0"
    scanner_dir = contents / "Resources/libexec"
    notices_dir = contents / "Resources/third-party-licenses"
    scanner = gst / "libexec/gstreamer-1.0/gst-plugin-scanner"
    roots = [(executable, executable), (scanner, scanner_dir / scanner.name)]
    roots += [(gst / f"lib/gstreamer-1.0/libgst{name}.dylib", plugin_dir / f"libgst{name}.dylib") for name in PLUGINS]
    roots.append((nice / "libexec/gstreamer-1.0/libgstnice.dylib", plugin_dir / "libgstnice.dylib"))
    for source, _ in roots:
        if not source.is_file():
            raise RuntimeError(f"Required runtime component missing: {source}")
    destinations = {source.resolve(): target for source, target in roots}
    occupied = {target: source for source, target in destinations.items()}
    executable = executable.resolve()
    executable_info = macho(executable)
    pending = list(destinations)
    details = {}
    links = {}
    while pending:
        source = pending.pop()
        if source in details:
            continue
        if FORBIDDEN.search(source.name):
            raise RuntimeError(f"Unexpected GPL/FFmpeg codec dependency: {source}")
        info = macho(source)
        if not executable_info["arches"].issubset(info["arches"]):
            raise RuntimeError(f"Architecture mismatch: {source} ({sorted(info['arches'])})")
        details[source] = info
        links[source] = []
        for dependency in info["deps"]:
            resolved = resolve_dependency(dependency, source, executable, info, executable_info)
            if resolved is None:
                continue
            if resolved not in destinations:
                target = framework_dir / resolved.name
                if target in occupied and occupied[target] != resolved:
                    raise RuntimeError(f"Two different libraries have the same packaged filename: {resolved.name}")
                destinations[resolved] = target
                occupied[target] = resolved
                pending.append(resolved)
            links[source].append((dependency, resolved))
    formulas = {}
    for source in details:
        formula = formula_for(source)
        if formula:
            name, version, prefix = formula
            formulas[(name, version)] = (prefix, license_files(prefix))
    absent = [name for (name, _), (_, files) in formulas.items() if not files]
    if absent:
        raise RuntimeError("Installed Homebrew formulas have no distributable license files: " + ", ".join(sorted(absent)))
    minimum = max([metadata.get("LSMinimumSystemVersion", "0.0")] + [info["minimum"] for info in details.values()], key=version_tuple)
    total_bytes = sum(source.stat().st_size for source in details)
    print(f"Runtime closure: {len(details)} Mach-O files, {total_bytes / 1024**2:.1f} MiB; {len(PLUGINS) + 1} plugins", flush=True)
    print(f"Architectures: {', '.join(sorted(executable_info['arches']))}; minimum macOS: {minimum}", flush=True)
    if args.dry_run:
        print("Dry run passed; no application files modified.")
        return
    for source, target in destinations.items():
        target.parent.mkdir(parents=True, exist_ok=True)
        if source != target.resolve():
            shutil.copy2(source, target)
        target.chmod(target.stat().st_mode | stat.S_IWUSR)
    for source, info in details.items():
        target = destinations[source]
        arguments = []
        if info["id"]:
            arguments += ["-id", "@rpath/" + target.name]
        for original, dependency in links[source]:
            relative = os.path.relpath(destinations[dependency], target.parent)
            arguments += ["-change", original, "@loader_path/" + relative]
        # Every non-system load is now relative; stale build-machine search paths
        # must not reintroduce Homebrew into the resulting application bundle.
        for rpath in info["rpaths"]:
            if Path(rpath).is_absolute() and not is_system(rpath):
                arguments += ["-delete_rpath", rpath]
        if arguments:
            command("/usr/bin/install_name_tool", *arguments, target)
    for target in destinations.values():
        info = macho(target)
        for dependency in info["deps"]:
            if is_system(dependency):
                continue
            if not dependency.startswith("@loader_path/"):
                raise RuntimeError(f"Nonportable dependency remains: {target}: {dependency}")
            resolved = Path(expand_loader(dependency, target, executable)).resolve()
            if not resolved.is_file() or not resolved.is_relative_to(contents):
                raise RuntimeError(f"Packaged dependency escapes or is missing: {target}: {dependency}")
    licenses = []
    for (name, version), (prefix, files) in sorted(formulas.items()):
        folder = notices_dir / f"{name}-{version}"
        for source in files:
            target = folder / source.relative_to(prefix)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
        # Homebrew's formula/SBOM identify upstream source versions, license
        # expressions and source-download locations without including user data.
        for source in [prefix / "sbom.spdx.json", prefix / ".brew" / f"{name}.rb"]:
            if source.is_file():
                shutil.copy2(source, folder / source.name)
        licenses.append({"formula": name, "version": version, "files": [str(source.relative_to(prefix)) for source in files]})
    project = Path(__file__).resolve().parent.parent
    for source, filename in [
        (project / "src-tauri/vendor/moonlight-common/LICENSE", "moonlight-common-LICENSE"),
        (project / "src-tauri/src/shaders/THIRD_PARTY_NOTICES.md", "shader-THIRD_PARTY_NOTICES.md"),
    ]:
        if not source.is_file():
            raise RuntimeError(f"Missing project third-party notice: {source}")
        notices_dir.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, notices_dir / filename)
    metadata["LSMinimumSystemVersion"] = minimum
    plist.write_bytes(plistlib.dumps(metadata, sort_keys=False))
    manifest = {"architectures": sorted(executable_info["arches"]), "minimumMacOS": minimum,
                "plugins": ["libgst" + name + ".dylib" for name in PLUGINS] + ["libgstnice.dylib"],
                "files": sorted(str(path.relative_to(contents)) for path in destinations.values()), "licenses": licenses}
    (contents / "Resources/gstreamer-runtime-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print("Portable runtime verified. Sign all nested Mach-O files, then the .app, before packaging.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, KeyError, ValueError) as error:
        print(f"Packaging failed: {error}", file=sys.stderr)
        sys.exit(1)
