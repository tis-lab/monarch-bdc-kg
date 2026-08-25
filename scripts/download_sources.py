#!/usr/bin/env python3
"""Mirror overlay source releases from the bucket.

Layout contract: each ingest publishes dated KGX releases under
gs://<bucket>/<name>/<YYYY-MM-DD>/ plus a latest/ mirror of the newest one.
Per source, `release:` from sources.yaml picks the dir: `latest` reads the
latest/ mirror, a YYYY-MM-DD pin reads that dated dir. The chosen dir is
mirrored to data/sources/<name>/ (deleting stale files, so the local mirror
always reflects exactly one release).

A `release-resolved.txt` is written into each mirror dir recording the date
actually used, for provenance in build logs.
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

import yaml

DATE_RE = re.compile(r"/(\d{4}-\d{2}-\d{2})/$")


def sh(args: list[str]) -> str:
    result = subprocess.run(args, capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(f"{' '.join(args)}: {result.stderr.strip()}")
    return result.stdout


def resolve_release(bucket: str, name: str, release: str) -> tuple[str, str]:
    """Return (dir_name_to_sync, resolved_note)."""
    listing = sh(["gcloud", "storage", "ls", f"{bucket}/{name}/"])
    dates = sorted(m.group(1) for line in listing.splitlines() if (m := DATE_RE.search(line.strip())))
    if release == "latest":
        if not dates:
            raise RuntimeError(f"no dated releases under {bucket}/{name}/")
        return "latest", f"latest (newest dated: {dates[-1]})"
    if release not in dates:
        raise RuntimeError(f"{bucket}/{name}/{release}/ not found; available: {', '.join(dates)}")
    return release, release


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--bucket", required=True, help="gs://bucket root")
    ap.add_argument("--sources", required=True, help="sources.yaml")
    ap.add_argument("--output-dir", required=True, help="local mirror root (data/sources)")
    args = ap.parse_args()

    sources = yaml.safe_load(open(args.sources))["sources"]
    for source in sources:
        name = source["name"]
        dirname, resolved = resolve_release(args.bucket, name, source.get("release", "latest"))
        dest = Path(args.output_dir) / name
        dest.mkdir(parents=True, exist_ok=True)
        sh(["gcloud", "storage", "rsync", "--delete-unmatched-destination-objects",
            f"{args.bucket}/{name}/{dirname}", str(dest)])
        (dest / "release-resolved.txt").write_text(f"{resolved}\n")
        print(f"{name}: {resolved} -> {dest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
