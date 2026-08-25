// BDC KG coverage dashboard: renders coverage.json (produced by
// scripts/coverage_report.py) as a goals-vs-actuals rubric matrix.

const fmt = (n) => n.toLocaleString("en-US");

const STATUS_LABEL = {
  met: "target met",
  gap: "GAP — target unmet",
  "not-target": "intentionally not a target",
  info: "",
};

async function main() {
  const resp = await fetch(`${import.meta.env.BASE_URL}coverage.json`);
  const doc = await resp.json();

  document.getElementById("meta").textContent =
    `generated ${doc.generated} · ${fmt(doc.nodes)} nodes · ${fmt(doc.edges)} edges`;

  renderStats(doc);
  renderMatrix(doc);
}

function renderStats(doc) {
  const el = document.getElementById("stats");
  const cards = [
    ["targets met", doc.targets_met, "met"],
    ["target gaps", doc.targets_gap, doc.targets_gap ? "gap" : "met"],
    ["edge sources", doc.provided_by.length, ""],
  ];
  el.innerHTML = cards
    .map(
      ([label, value, cls]) =>
        `<div class="card ${cls}"><div class="value">${fmt(value)}</div><div class="label">${label}</div></div>`
    )
    .join("");
}

function renderMatrix(doc) {
  const types = [...doc.type_order, "(other)"];
  const byPair = new Map(doc.pairs.map((p) => [`${p.subject_type}|${p.object_type}`, p]));

  let html = "<table><thead><tr><th>subject ↓ object →</th>";
  for (const t of types) html += `<th>${t}</th>`;
  html += "</tr></thead><tbody>";

  for (const st of types) {
    html += `<tr><th>${st}</th>`;
    for (const ot of types) {
      const p = byPair.get(`${st}|${ot}`);
      if (!p) {
        html += `<td class="empty"></td>`;
        continue;
      }
      const cls = p.status === "gap" ? "gap" : p.status === "met" ? "met" : p.status === "not-target" ? "nt" : p.total ? "pop" : "zero";
      const title = STATUS_LABEL[p.status] || "";
      html += `<td class="${cls}" data-pair="${st}|${ot}" title="${title}">${p.total ? fmt(p.total) : "0"}</td>`;
    }
    html += "</tr>";
  }
  html += "</tbody></table>";
  const el = document.getElementById("matrix");
  el.innerHTML = html;

  el.querySelectorAll("td[data-pair]").forEach((td) =>
    td.addEventListener("click", () => renderDetail(byPair.get(td.dataset.pair)))
  );
}

function renderDetail(p) {
  const sec = document.getElementById("detail");
  sec.hidden = false;
  document.getElementById("detail-title").textContent =
    `${p.subject_type} → ${p.object_type} (${fmt(p.total)} edges${p.status !== "info" ? ", " + STATUS_LABEL[p.status] : ""})`;
  document.getElementById("detail-note").textContent = p.goal_note || "";
  const rows = p.predicates
    .map(
      (x) =>
        `<tr><td>${x.predicate}</td><td class="num">${fmt(x.count)}</td><td>${x.sources.join(", ")}</td></tr>`
    )
    .join("");
  document.getElementById("detail-table").innerHTML =
    `<thead><tr><th>predicate</th><th>edges</th><th>primary knowledge sources</th></tr></thead><tbody>${rows || "<tr><td colspan=3>no edges</td></tr>"}</tbody>`;
  sec.scrollIntoView({ behavior: "smooth", block: "nearest" });
}

main().catch((e) => {
  document.getElementById("meta").textContent = `failed to load coverage.json: ${e}`;
});
