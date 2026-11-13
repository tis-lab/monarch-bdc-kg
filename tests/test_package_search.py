"""scripts/package_search.py: what a search release contains and what its manifest says."""

import importlib.util
import json
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("package_search", ROOT / "scripts" / "package_search.py")
package_search = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package_search)

INFO = {"bdc_search": "0.1.0", "schema_version": 2, "index_version": "abc123", "entities": 3, "concepts": 2}


def test_a_release_packages_the_index_with_its_manifest(tmp_path):
    index = tmp_path / "index"
    index.mkdir()
    (index / "meta.json").write_text("{}")
    (index / "concepts-meta.json").write_text(json.dumps({"revision": "3260ff3", "generated": "2026-11-12"}))
    kg = tmp_path / "bdc-kg.duckdb"
    kg.write_bytes(b"duckdb")

    published = package_search.package(index, tmp_path / "out", "2026-11-13", kg, INFO)

    assert published["kg_release"] == "2026-11-13"
    assert published["hv_revision"] == "3260ff3"
    assert published["index"] == {"schema_version": 2, "index_version": "abc123", "entities": 3, "concepts": 2}
    assert published["tantivy"], "read from search/Cargo.lock"
    assert published["artifact"]["sha256"] == package_search.sha256(tmp_path / "out" / "index.tar.gz")

    with tarfile.open(tmp_path / "out" / "index.tar.gz") as tar:
        names = set(tar.getnames())
        inside = json.load(tar.extractfile("index/manifest.json"))
    assert {"index/meta.json", "index/concepts-meta.json", "index/manifest.json"} <= names
    assert inside["index"]["index_version"] == "abc123", "the server's /version reads this copy"
    assert "artifact" not in inside, "a tarball can't contain its own checksum"
    assert json.loads((tmp_path / "out" / "manifest.json").read_text()) == published
