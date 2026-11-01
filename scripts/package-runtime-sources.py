#!/usr/bin/env python3
"""Archive checksum-verified corresponding sources of a packaged macOS runtime.

Usage: python3 scripts/package-runtime-sources.py 'path/to/KVM Client.app'
No Homebrew bottles, authentication headers, credentials or user configuration
are read. Downloads use only public HTTPS URLs from shipped recipes and SBOMs.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import hashlib
import json
from pathlib import Path
import plistlib
import re
import shutil
import ssl
import tarfile
import tempfile
from urllib.parse import unquote, urlparse
from urllib.request import HTTPRedirectHandler, HTTPSHandler, ProxyHandler, Request, build_opener


def valid_url(url):
    parsed = urlparse(url)
    if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password:
        raise ValueError(f"Source URL must be public HTTPS without credentials: {url}")
    if parsed.hostname in {"localhost", "127.0.0.1", "::1"} or parsed.hostname.endswith(".local"):
        raise ValueError(f"Local source URLs are not accepted: {url}")
    return url


class HTTPSRedirects(HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        valid_url(new_url)
        return super().redirect_request(request, response, code, message, headers, new_url)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def recipe_sources(text):
    # Ruby's standard Homebrew formatting gives these DSL blocks matching
    # indentation. Exclude nonrelease HEAD, bottle and URL-discovery metadata.
    for block in ["head", "bottle", "livecheck", "test", "on_linux"]:
        pattern = rf"^(?P<indent> *){block} do[^\n]*\n.*?^(?P=indent)end\b[^\n]*(?:\n|$)"
        text = re.sub(pattern, "", text, flags=re.M | re.S)
    result = []
    lines = text.splitlines()
    for index, line in enumerate(lines):
        match = re.match(r'^\s+url "(https://[^\"]+)"', line)
        if not match:
            continue
        for following in lines[index + 1:index + 8]:
            checksum = re.match(r'^\s+sha256 "([0-9a-f]{64})"\s*$', following)
            if checksum:
                result.append((valid_url(match.group(1)), checksum.group(1)))
                break
            if re.match(r"^\s*(url|resource|patch|end)\b", following):
                break
    return result


def plan(contents, additional_sources):
    notices = contents / "Resources/third-party-licenses"
    runtime = json.loads((contents / "Resources/gstreamer-runtime-manifest.json").read_text())
    downloads = {}
    formulas = []
    unresolved = []

    def add(url, checksum, formula, role, recipe_file=None, installed_version=None):
        valid_url(url)
        if not re.fullmatch(r"[0-9a-fA-F]{64}", checksum):
            raise ValueError(f"Missing SHA256 for {formula}: {url}")
        checksum = checksum.lower()
        item = downloads.setdefault(checksum, {"sha256": checksum, "urls": [], "consumers": []})
        if url not in item["urls"]:
            item["urls"].append(url)
        consumer = {"formula": formula, "role": role}
        if recipe_file:
            consumer.update(recipeFile=recipe_file, installedVersion=installed_version)
        if consumer not in item["consumers"]:
            item["consumers"].append(consumer)

    for entry in runtime["licenses"]:
        name, installed_version = entry["formula"], entry["version"]
        directory = notices / f"{name}-{installed_version}"
        recipe = directory / f"{name}.rb"
        sbom = directory / "sbom.spdx.json"
        if not recipe.is_file() or not sbom.is_file():
            raise ValueError(f"Exact build recipe/SBOM missing for {name}-{installed_version}")
        packages = json.loads(sbom.read_text())["packages"]
        own = [package for package in packages if package["SPDXID"].startswith("SPDXRef-Archive-")]
        if len(own) != 1:
            raise ValueError(f"Expected one own-source package in {sbom}")
        source_version = own[0].get("versionInfo")
        if source_version != re.sub(r"_\d+$", "", installed_version):
            raise ValueError(f"Source/binary version mismatch for {name}: {source_version} != {installed_version}")
        for package in packages:
            identifier = package["SPDXID"]
            if not identifier.startswith(("SPDXRef-Archive-", "SPDXRef-Patch-")):
                continue
            checksums = [value["checksumValue"] for value in package.get("checksums", []) if value["algorithm"] == "SHA256"]
            if len(checksums) != 1:
                raise ValueError(f"Exact source SHA256 missing for {identifier}")
            add(package["downloadLocation"], checksums[0], name, "source" if "Archive" in identifier else "patch")
        for url, checksum in recipe_sources(recipe.read_text()):
            add(url, checksum, name, "stable-recipe-source-or-patch")
        for filename in re.findall(r'^\s+file "([^\"]+)"', recipe.read_text(), flags=re.M):
            candidates = [item for item in additional_sources
                          if item.get("formula") == name and item.get("version") == installed_version
                          and item.get("file") == filename]
            if len(candidates) != 1:
                unresolved.append(f"{name}-{installed_version}: {filename}")
            else:
                item = candidates[0]
                add(item["url"], item["sha256"], name, "pinned-recipe-local-patch", filename, installed_version)
        formulas.append({"formula": name, "installedVersion": installed_version, "sourceVersion": source_version})
    return notices, runtime, formulas, list(downloads.values()), unresolved


def download(item, cache):
    basename = unquote(Path(urlparse(item["urls"][0]).path).name)
    basename = re.sub(r"[^A-Za-z0-9._+-]", "_", basename)[:140] or "source"
    filename = item["sha256"][:12] + "-" + basename
    target = cache / filename
    if target.is_file() and sha256(target) == item["sha256"]:
        return item, target, "cached"
    errors = []
    for url in item["urls"]:
        # Disable ambient proxy/authentication configuration and .netrc use.
        opener = build_opener(ProxyHandler({}), HTTPSHandler(context=ssl.create_default_context()), HTTPSRedirects())
        temporary = target.with_suffix(target.suffix + ".partial")
        try:
            request = Request(url, headers={"User-Agent": "kvm-client-runtime-source-packager/1.0"})
            with opener.open(request, timeout=45) as response, temporary.open("wb") as stream:
                valid_url(response.url)
                shutil.copyfileobj(response, stream, length=1024 * 1024)
            actual = sha256(temporary)
            if actual != item["sha256"]:
                raise ValueError(f"SHA256 mismatch: expected {item['sha256']}, got {actual}")
            temporary.replace(target)
            return item, target, "downloaded"
        except Exception as error:
            errors.append(f"{url}: {error}")
            temporary.unlink(missing_ok=True)
    raise RuntimeError("Unable to obtain exact verified source:\n" + "\n".join(errors))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--plan", action="store_true", help="Validate metadata and print the source list without downloading")
    parser.add_argument("--download-only", action="store_true", help="Populate the verified cache; never create an archive")
    parser.add_argument("--additional-sources", type=Path, help="JSON array of pinned recipe-local patches: formula, version, file, url, sha256")
    args = parser.parse_args()
    contents = args.app.resolve() / "Contents"
    app_metadata = plistlib.loads((contents / "Info.plist").read_bytes())
    version = app_metadata["CFBundleShortVersionString"]
    project = Path(__file__).resolve().parent.parent
    output = args.output or project / f".tools/releases/v{version}/runtime-sources.tar.gz"
    additional_sources = json.loads(args.additional_sources.read_text()) if args.additional_sources else []
    notices, runtime, formulas, downloads, unresolved = plan(contents, additional_sources)
    print(f"Verified source metadata: {len(formulas)} formulas, {len(downloads)} unique source/patch downloads", flush=True)
    if unresolved and not args.download_only:
        raise ValueError("Unpinned recipe-local patches require --additional-sources: " + "; ".join(unresolved))
    for missing in unresolved:
        print("Archive blocked until patch is pinned: " + missing, flush=True)
    if args.plan:
        for item in downloads:
            print(item["sha256"], item["urls"][0], flush=True)
        return
    output.parent.mkdir(parents=True, exist_ok=True)
    cache = project / ".tools/runtime-source-downloads"
    cache.mkdir(parents=True, exist_ok=True)
    obtained = []
    with ThreadPoolExecutor(max_workers=4) as executor:
        futures = [executor.submit(download, item, cache) for item in downloads]
        for future in as_completed(futures):
            item, path, status = future.result()
            print(f"{status}: {path.name} ({path.stat().st_size / 1024**2:.2f} MiB)", flush=True)
            obtained.append((item, path))
    if args.download_only:
        print("Verified source cache populated; no archive created.", flush=True)
        return
    with tempfile.TemporaryDirectory(prefix="runtime-sources-", dir=output.parent) as temporary:
        stage = Path(temporary) / "runtime-sources"
        (stage / "archives").mkdir(parents=True)
        shutil.copytree(notices, stage / "build-recipes-and-notices")
        shutil.copy2(contents / "Resources/gstreamer-runtime-manifest.json", stage / "gstreamer-runtime-manifest.json")
        for item, path in obtained:
            shutil.copy2(path, stage / "archives" / path.name)
            item["file"] = "archives/" + path.name
            item["bytes"] = path.stat().st_size
            for consumer in item["consumers"]:
                if "recipeFile" in consumer:
                    relative = Path(consumer["recipeFile"])
                    if relative.is_absolute() or ".." in relative.parts:
                        raise ValueError("Recipe-local patch path must stay within its formula directory")
                    target = stage / "build-recipes-and-notices" / (consumer["formula"] + "-" + consumer["installedVersion"]) / relative
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(path, target)
        if args.additional_sources:
            shutil.copy2(args.additional_sources, stage / "additional-source-pins.json")
        for filename in ["package-macos-runtime.py", "package-runtime-sources.py"]:
            shutil.copy2(project / "scripts" / filename, stage / filename)
        manifest = {"applicationVersion": version, "runtimeArchitectures": runtime["architectures"],
                    "formulas": formulas, "sources": sorted(downloads, key=lambda item: item["file"])}
        (stage / "sources-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        (stage / "README.md").write_text(
            "# Corresponding native runtime sources\n\n"
            f"These are the exact sources for the native libraries/plugins shipped with KVM Client {version}. "
            "Every download was verified against SHA256 from the packaged Homebrew SPDX metadata or exact build recipe. "
            "The source manifest maps shared archives to their consuming formulas and records every checksum.\n\n"
            "`archives/` contains unmodified upstream source releases, supplementary build resources and external patches. "
            "`build-recipes-and-notices/` preserves the installed versions' Homebrew build scripts, inline patches, "
            "compiler/dependency metadata and license notices. Recipes specify configuration flags and patch application; "
            "their SBOMs identify the build toolchain. HEAD development sources and binary bottle dependencies are excluded.\n\n"
            "The included runtime packaging script reproduces dylib relocation and plugin selection after building "
            "the corresponding formulas. Application Rust/TypeScript code, Cargo/npm lockfiles and the vendored "
            "Moonlight source are provided separately in the matching application-source release archive.\n"
        )
        temporary_archive = output.with_suffix(output.suffix + ".partial")
        def anonymous_owner(info):
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            return info
        with tarfile.open(temporary_archive, "w:gz") as archive:
            archive.add(stage, arcname="runtime-sources", filter=anonymous_owner)
        temporary_archive.replace(output)
    print(f"Created {output} ({output.stat().st_size / 1024**2:.2f} MiB), SHA256 {sha256(output)}", flush=True)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, RuntimeError) as error:
        raise SystemExit(f"Runtime source packaging failed: {error}")
