#!/usr/bin/env python3
"""Fetch the pinned official yt-dlp onedir runtime onto the external SSD. No app package is built."""
import hashlib, json, os, shutil, subprocess, tempfile, urllib.request, zipfile
from pathlib import Path, PurePosixPath
PROJECT = Path(__file__).resolve().parent.parent
STORAGE = PROJECT.parent.parent
VERSION = "2026.08.19"
SHA256 = "30b4c14aafab6082becff7881e41b76df46dc43ea7633479410a91e29da492bf"
URL = f"https://github.com/yt-dlp/yt-dlp/releases/download/{VERSION}/yt-dlp_win.zip"

def unpack(archive, destination):
    with zipfile.ZipFile(archive) as z:
        total = 0
        for member in z.infolist():
            path = PurePosixPath(member.filename)
            total += member.file_size
            if (path.is_absolute() or ".." in path.parts or "\\" in member.filename
                or ":" in member.filename or (member.external_attr >> 16) & 0o170000 == 0o120000
                or total > 128 * 1024 * 1024):
                raise RuntimeError("Invalid helper archive")
        z.extractall(destination)
    if not (destination / "yt-dlp.exe").is_file() or not (destination / "_internal/python310.dll").is_file():
        # The internal Python version can change only when the pinned release is reviewed.
        raise RuntimeError("Incomplete pinned onedir runtime")

def main():
    subprocess.run([str(Path.home()/".local/bin/rust-external"),"--paths"],check=True,stdout=subprocess.DEVNULL)
    helpers = STORAGE / "windows/helpers"
    destination = helpers / "yt-dlp"
    marker = {"version":VERSION,"archive_sha256":SHA256,"source":URL,"layout":"onedir"}
    if (destination/"runtime-source.json").is_file():
        if json.loads((destination/"runtime-source.json").read_text()) == marker:
            if (destination/"yt-dlp.exe").is_file() and (destination/"_internal/python310.dll").is_file():
                print("Pinned unpacked Windows helper already prepared")
                return
        raise RuntimeError("Existing helper runtime differs; review before replacing it")
    if destination.exists():
        raise RuntimeError("Existing unverified helper runtime; review before replacing it")
    with tempfile.TemporaryDirectory(dir=STORAGE/"tmp",prefix="yt-dlp-onedir-") as temp:
        temp=Path(temp); archive=temp/"helper.zip"
        cached_archive = helpers / "yt-dlp-onedir.zip"
        if cached_archive.is_file():
            shutil.copy2(cached_archive, archive)
        else:
            with urllib.request.urlopen(URL,timeout=60) as source, archive.open("wb") as output:
                total = 0
                while block := source.read(1024*1024):
                    total += len(block)
                    if total > 32*1024*1024:
                        raise RuntimeError("Helper download exceeds expected size")
                    output.write(block)
        with archive.open("rb") as source:
            digest=hashlib.file_digest(source,"sha256").hexdigest()
        if digest != SHA256:
            raise RuntimeError("Official helper checksum mismatch")
        stage=temp/"runtime";stage.mkdir();unpack(archive,stage)
        (stage/"runtime-source.json").write_text(json.dumps(marker,indent=2)+"\n")
        os.replace(stage,destination)
    print("Prepared checksum-verified unpacked Windows helper:", destination)

if __name__ == "__main__": main()
