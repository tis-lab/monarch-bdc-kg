"""Package the search index for a KG release: OUT_DIR/index.tar.gz and OUT_DIR/manifest.json.

`just search-release` runs this into output/search/, which `just upload` publishes beside the
KG as kg/{release}/search/. The manifest says what the index was built from and by what, so a
server or a reader can tell which KG, which harmonization specs and which bdc-search it pairs
with. A copy goes into the index directory too, where the server reports it at /version.
"""

import argparse
import datetime
import hashlib
import json
import re
import subprocess
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def git_state() -> dict:
    sha = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    dirty = subprocess.run(["git", "-C", str(ROOT), "status", "--porcelain", "--", "search", "sql", "scripts"],
                           capture_output=True, text=True).stdout.strip()
    return {"git_sha": sha or None, "git_dirty": bool(dirty)}


def tantivy_version() -> str | None:
    lock = (ROOT / "search" / "Cargo.lock").read_text()
    found = re.search(r'name = "tantivy"\nversion = "([^"]+)"', lock)
    return found.group(1) if found else None


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def package(index_dir: Path, out_dir: Path, kg_release: str, kg_db: Path, info: dict) -> dict:
    """Write the manifest into the index, tar the index, and write the manifest beside the tar."""
    meta_path = index_dir / "concepts-meta.json"
    concepts_meta = json.loads(meta_path.read_text()) if meta_path.exists() else {}
    manifest = {
        "kg_release": kg_release,
        "kg_duckdb": {"file": kg_db.name, "bytes": kg_db.stat().st_size if kg_db.exists() else None},
        "hv_revision": concepts_meta.get("revision"),
        "packaged_at": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "bdc_search": {"version": info.get("bdc_search"), **git_state()},
        "tantivy": tantivy_version(),
        "index": {
            "schema_version": info.get("schema_version"),
            "index_version": info.get("index_version"),
            "entities": info.get("entities"),
            "concepts": info.get("concepts"),
        },
    }
    (index_dir / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")

    out_dir.mkdir(parents=True, exist_ok=True)
    tarball = out_dir / "index.tar.gz"
    with tarfile.open(tarball, "w:gz") as tar:
        tar.add(index_dir, arcname="index")
    published = {**manifest, "artifact": {"file": tarball.name, "bytes": tarball.stat().st_size, "sha256": sha256(tarball)}}
    (out_dir / "manifest.json").write_text(json.dumps(published, indent=2) + "\n")
    return published


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("index_dir", type=Path)
    parser.add_argument("out_dir", type=Path)
    parser.add_argument("--kg-release", required=True, help="the KG release date, as in kg/{release}/")
    parser.add_argument("--kg-db", type=Path, required=True)
    parser.add_argument("--binary", type=Path, default=ROOT / "search" / "target" / "release" / "bdc-search")
    args = parser.parse_args()

    # What the server will report: the index version and counts, read by bdc-search itself.
    info = json.loads(subprocess.run([str(args.binary), "info", "--index-dir", str(args.index_dir)],
                                     capture_output=True, text=True, check=True).stdout)
    published = package(args.index_dir, args.out_dir, args.kg_release, args.kg_db, info)
    print(f"  {args.out_dir}/index.tar.gz: {published['artifact']['bytes'] / 1e6:.0f} MB, "
          f"index {published['index']['index_version']}, {published['index']['entities']} entities, "
          f"{published['index']['concepts']} concepts, KG {published['kg_release']}")


if __name__ == "__main__":
    main()
