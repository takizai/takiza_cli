"""Populate npm/vendor from the matching Git tag and verify GitHub asset digests."""
from concurrent.futures import ThreadPoolExecutor
import hashlib
import io
import json
from pathlib import Path
import tarfile
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parent
package = json.loads((ROOT / "package.json").read_text())
TAG = "v" + package["version"]
TARGETS = {
    "x86_64-unknown-linux-gnu": ".tar.gz",
    "x86_64-unknown-linux-musl": ".tar.gz",
    "aarch64-unknown-linux-gnu": ".tar.gz",
    "x86_64-apple-darwin": ".tar.gz",
    "aarch64-apple-darwin": ".tar.gz",
    "x86_64-pc-windows-msvc": ".zip",
}


def release_assets():
    request = urllib.request.Request(
        f"https://api.github.com/repos/takizai/takiza_cli/releases/tags/{TAG}",
        headers={"User-Agent": "takiza-npm-packager", "Accept": "application/vnd.github+json"})
    with urllib.request.urlopen(request, timeout=90) as response:
        release = json.load(response)
    assert release["tag_name"] == TAG and not release["draft"] and not release["prerelease"], "Release is not ready"
    expected = {f"takiza-{TAG}-{target}{suffix}" for target, suffix in TARGETS.items()}
    assets = [asset for asset in release["assets"] if asset["name"] in expected]
    assert len(assets) == len(expected) and {asset["name"] for asset in assets} == expected, "Release binaries are incomplete"
    return sorted(assets, key=lambda asset: asset["name"])


def prepare(asset):
    name = asset["name"]
    digest = asset["digest"]
    assert digest.startswith("sha256:"), "Missing release checksum"
    request = urllib.request.Request(asset["browser_download_url"], headers={"User-Agent": "takiza-npm-packager"})
    with urllib.request.urlopen(request, timeout=90) as response:
        data = response.read()
    assert "sha256:" + hashlib.sha256(data).hexdigest() == digest, f"Checksum mismatch: {name}"
    if name.endswith(".tar.gz"):
        suffix, binary = ".tar.gz", "takiza"
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
            members = [member for member in archive.getmembers() if member.isfile() and Path(member.name).name == binary]
            assert len(members) == 1, f"Binary missing or ambiguous: {name}"
            content = archive.extractfile(members[0]).read()
    elif name.endswith(".zip"):
        suffix, binary = ".zip", "takiza.exe"
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            members = [member for member in archive.infolist() if not member.is_dir() and Path(member.filename).name == binary]
            assert len(members) == 1, f"Binary missing or ambiguous: {name}"
            content = archive.read(members[0])
    else:
        raise ValueError(f"Unsupported archive: {name}")
    target = name.removeprefix("takiza-" + TAG + "-").removesuffix(suffix)
    destination = ROOT / "vendor" / target / binary
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(content)
    destination.chmod(0o755)
    print(f"Prepared {target}: {len(content)} bytes", flush=True)
    return {"version": package["version"], "target": target, "archive_sha256": digest.removeprefix("sha256:"),
            "binary_sha256": hashlib.sha256(content).hexdigest()}


if __name__ == "__main__":
    with ThreadPoolExecutor(max_workers=6) as workers:
        checksums = list(workers.map(prepare, release_assets()))
    (ROOT / "vendor" / "checksums.json").write_text(json.dumps(checksums, indent=2) + "\n")
