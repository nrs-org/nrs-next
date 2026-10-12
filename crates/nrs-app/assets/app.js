// @ts-check

const app = document.querySelector("#app");
if (!(app instanceof HTMLElement)) throw new Error("missing #app");

const sections = {
  profile: ["Profile", "Baseline craft and progress: what kind of entry this is and how much of it was consumed."],
  responses: ["Responses", "Specific moments and aftereffects. Repeated NEI/AEI kinds are folded into one glanceable group."],
  resonance: ["Personal resonance", "Attachments, memes, gateways, and other effects that escaped the work itself."],
  adjustments: ["Adjustments", "Explicit score corrections kept separate from observed responses."],
  other: ["Other", "Custom or legacy impacts without a standards category."],
};

// Each section lists its labels in a fixed order (container before
// contained, original before derived) so groups don't reshuffle
// depending on which relation happens to come back first.
const relationSections = {
  structure: ["Structure", "Collections, albums, franchises, and their members.", ["Contains", "Contained by"]],
  authorship: ["Authorship", "Who made it and where their work appears.", ["Credited on", "Contributors"]],
  lineage: ["Lineage", "Originals, remixes, and derived versions.", ["Remix of", "Remixed into"]],
  influence: ["Influence", "Music carried into another entry, or value displaced by another work.", ["Features music", "Featured in", "Killed by", "Kills"]],
};

let requestController = new AbortController();
let currentEntryData = null;
let searchTimer;
let routeVersion = 0;
// Per-page hooks, reset on every navigation: `pageTeardown` releases
// whatever the page set up (observers), `pageKeys` handles shortcuts.
let pageTeardown = null;
let pageKeys = null;

// [kind, label, section] -- section mirrors the server's
// `impact_section`, and groups the kind picker the same way the entry
// page groups impacts.
const impactKinds = [
  ["cry", "Cry", "responses"],
  ["nei", "Noticeable emotional impact (NEI)", "responses"],
  ["aei", "Appreciable emotional impact (AEI)", "responses"],
  ["ehi", "Exceptional humor impact (EHI)", "responses"],
  ["epi", "Exceptional plot impact (EPI)", "responses"],
  ["jumpscare", "Jumpscare", "responses"],
  ["sleepless_night", "Sleepless night", "responses"],
  ["politics", "Politics", "resonance"],
  ["interest_field", "New interest field (gateway)", "resonance"],
  ["additional", "Additional (buff/nerf)", "adjustments"],
  ["music", "Music", "profile"],
  ["visual", "Visual", "profile"],
  ["writing", "Writing quality", "profile"],
  ["osu_song", "osu! song", "resonance"],
  ["waifu", "Waifu", "resonance"],
  ["meme", "Meme", "resonance"],
  ["pads", "PADS", "responses"],
  ["consumed", "Consumed (boredom)", "profile"],
  ["anime_consumed", "Anime consumed (boredom, by episode)", "profile"],
  ["dropped", "Dropped", "profile"],
  ["music_consumed_progress", "Music consumed progress", "profile"],
];

const relationKinds = [
  ["contains", "Contains"],
  ["remix", "Remix"],
  ["feature_music", "Feature music"],
  ["killed_by", "Killed-by"],
];

const anchors = [
  "comedy", "hype", "tragedy", "wistful", "cozy", "heartwarming", "hate",
  "discomfort", "waifu", "meme", "hooked", "worldview", "gateway", "writing",
  "visual", "music", "boredom", "additional",
];

// The server's `entry_kind` slugs, in chip order, with display labels.
const entryKinds = {
  anime: "anime",
  album: "album",
  track: "track",
  artist: "artist",
  franchise: "franchise",
  game: "game",
  vn: "visual novel",
  literature: "literature",
  other: "other",
};

function kindTag(kind) {
  return `<span class="kind-tag" data-kind="${escapeHtml(kind)}">${escapeHtml(entryKinds[kind] || kind)}</span>`;
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function pathForEntry(id) {
  return `/entries/${encodeURIComponent(id)}`;
}

async function getJson(path) {
  const version = routeVersion;
  requestController.abort();
  requestController = new AbortController();
  const response = await fetch(path, {
    signal: requestController.signal,
    headers: { Accept: "application/json" },
  });
  const body = await response.json().catch(() => ({}));
  if (version !== routeVersion) throw new DOMException("Navigation changed", "AbortError");
  if (!response.ok) throw new Error(body.error || `${response.status} ${response.statusText}`);
  return body;
}

async function sendJson(path, method, body) {
  const response = await fetch(path, {
    method,
    headers: { Accept: "application/json", "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(payload.error || `${response.status} ${response.statusText}`);
  return payload;
}

function showToast(message, tone = "ok") {
  const toast = document.querySelector("#toast");
  if (!(toast instanceof HTMLElement)) return;
  toast.textContent = message;
  toast.dataset.tone = tone;
  toast.classList.add("visible");
  setTimeout(() => toast.classList.remove("visible"), 2200);
}

function navigate(url) {
  history.pushState(null, "", url);
  route();
}

document.addEventListener("click", (event) => {
  if (!(event.target instanceof Element)) return;
  const action = event.target.closest("[data-action]");
  if (action instanceof HTMLElement) {
    const name = action.dataset.action;
    if (name === "add-impact") openImpactEditor(null, action);
    if (name === "edit-impact") {
      const impact = currentEntryData?.impacts.find((item) => item.id === action.dataset.id);
      if (impact) impact.kind ? openImpactEditor(impact, action) : openRawImpactEditor(impact, action);
    }
    if (name === "confirm-impact") confirmImpact(action.dataset.id);
    if (name === "add-relation") openRelationEditor(null, action);
    if (name === "edit-relation") {
      const relation = currentEntryData?.relations.find((item) => item.id === action.dataset.id);
      if (relation) openRelationEditor(relation, action);
    }
    if (name === "delete-relation") deleteRelation(action.dataset.id);
    if (name === "close-editor") closeEditor(action);
    return;
  }
  const anchor = event.target.closest("a[data-link]");
  if (!(anchor instanceof HTMLAnchorElement)) return;
  if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
  if (anchor.origin !== location.origin) return;
  event.preventDefault();
  navigate(anchor.href);
});

window.addEventListener("popstate", route);

document.addEventListener("keydown", (event) => {
  if (!pageKeys || event.metaKey || event.ctrlKey || event.altKey) return;
  const target = event.target;
  const typing = target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement;
  pageKeys(event, typing);
});

app.addEventListener("toggle", (event) => {
  const details = event.target;
  if (!(details instanceof HTMLDetailsElement) || !details.classList.contains("card-details") || !details.open || !details.isConnected || details.querySelector(".inline-editor-panel")) return;
  const button = details.querySelector('[data-action="edit-impact"]');
  if (button instanceof HTMLButtonElement) button.click();
}, true);

function setActiveNav() {
  const segment = location.pathname.split("/")[1];
  const active = ["palette", "calibrate", "kfeat", "review", "entries"].includes(segment)
    ? segment
    : "scores";
  document.querySelectorAll(".rail nav a[data-nav]").forEach((anchor) => {
    anchor.classList.toggle("active", anchor.getAttribute("data-nav") === active);
  });
}

function renderError(error) {
  if (error?.name === "AbortError") return;
  app.innerHTML = `<p class="error">${escapeHtml(error?.message || error)}</p>`;
}

const SCORE_PAGE_SIZE = 100;

function deltaMarkup(delta) {
  if (delta == null) return `<span class="dim">new</span>`;
  if (delta > 0) return `<span class="delta-up">▲${delta}</span>`;
  if (delta < 0) return `<span class="delta-down">▼${-delta}</span>`;
  return `<span class="dim">–</span>`;
}

/** `(+3)`-style overall Δ, shown beside the filtered one. */
function overallDelta(delta) {
  if (delta == null) return "new";
  return delta > 0 ? `+${delta}` : delta < 0 ? `−${-delta}` : "0";
}

/**
 * `filtered`: the view is narrowed by a filter, so rank and Δ are shown
 * among the matches (1..N) with the overall figures beside them.
 */
function scoreRow(row, comparing, filtered) {
  const rank = filtered ? `${row.filtered_rank}<small>(${row.rank})</small>` : row.rank;
  const delta = filtered
    ? `${deltaMarkup(row.filtered_delta)}<small>(${overallDelta(row.delta)})</small>`
    : deltaMarkup(row.delta);
  return `<tr data-id="${escapeHtml(row.id)}" data-kind="${escapeHtml(row.kind)}">
    <td class="col-rank">${rank}</td>
    ${comparing ? `<td class="col-delta">${delta}</td>` : ""}
    <td class="col-title"><a href="${pathForEntry(row.id)}" data-link>${escapeHtml(row.title || row.id)}</a></td>
    <td class="col-type">${kindTag(row.kind)}</td>
    <td class="col-store">${escapeHtml(row.store)}</td>
    <td class="col-value">${Number(row.value).toFixed(4)}</td>
  </tr>`;
}

/**
 * The ranking, paged: rows arrive SCORE_PAGE_SIZE at a time as the
 * end of the table scrolls into view. Sorting and filtering happen on
 * the server against a cached ranking, so they only refetch the first
 * page. A rerun with new parameters compares ranks against the run
 * before it.
 */
async function renderScores() {
  currentEntryData = null;
  document.title = "Scores — NRS";
  const params = new URLSearchParams(location.search);
  const view = {
    sort: params.get("sort") || "value",
    dir: params.get("dir") || "desc",
    q: params.get("q") || "",
    cmp_gen: params.get("cmp_gen") || "",
  };
  const comparing = Boolean(view.cmp_gen);
  const columns = [
    ["rank", "#", "value"],
    ...(comparing ? [["delta", "Δ", "delta"]] : []),
    ["title", "title", "title"],
    ["type", "type", "type"],
    ["store", "store", "id"],
    ["value", "V (raw)", "value"],
  ];
  app.innerHTML = `
    <h1>scores</h1>
    <p class="muted">Uncalibrated V (nested pooling over <a href="/kfeat">K</a>) -- not a public score yet.</p>
    <div class="ledger-meta">
      <span class="score-count">Calculating…</span>
      ${comparing ? `<span class="compare-note">Δ vs version ${escapeHtml(view.cmp_gen)} <button class="edit-link" type="button" data-clear-compare>clear</button></span>` : `<span class="compare-note" hidden><button class="edit-link" type="button" data-start-compare>compare with the previous version</button></span>`}
      <a class="csv-link">download csv</a>
    </div>
    <div class="toolbar score-toolbar">
      <input class="search" type="search" placeholder="filter… try  type:anime v>10  ( / )" value="${escapeHtml(view.q)}" spellcheck="false" autocomplete="off">
      <details class="filter-help"><summary>syntax</summary><div class="filter-help-body">
        <p>Space-separated terms; every one must match. A leading <code>-</code> negates any term.</p>
        <dl>
          <dt><code>word</code> <code>"a phrase"</code></dt><dd>title, id, or store contains it</dd>
          <dt><code>type:anime</code></dt><dd>${Object.keys(entryKinds).join(", ")}, or <code>music</code> for album + track + artist — several mean any of them (click a chip; shift-click excludes)</dd>
          <dt><code>store:mal</code></dt><dd>store contains it</dd>
          <dt><code>v>10</code> <code>rank<=100</code></dt><dd>compare with <code>&gt; &gt;= &lt; &lt;= =</code></dd>
          <dt><code>delta>=5</code> <code>is:up</code> <code>is:down</code> <code>is:moved</code></dt><dd>rank changes since the compared version (after an edit)</dd>
        </dl>
      </div></details>
    </div>
    <div class="kind-chips" aria-label="filter by type"></div>
    <p class="filter-warnings" hidden></p>
    <div class="score-table-wrap">
      <table class="score-table">
        <thead><tr>${columns.map(([key, label, sort]) => `<th class="col-${key}">${sort && key !== "rank" ? `<button class="sort-button" type="button" data-sort="${sort}">${label}</button>` : label}</th>`).join("")}</tr></thead>
        <tbody></tbody>
      </table>
      <div class="score-sentinel"></div>
    </div>
    <p class="key-hint dim">j / k to move · enter to open · / to filter</p>`;
  const count = app.querySelector(".score-count");
  const csv = app.querySelector(".csv-link");
  const search = app.querySelector(".search");
  const table = app.querySelector(".score-table");
  const tbody = app.querySelector(".score-table tbody");
  const sentinel = app.querySelector(".score-sentinel");
  const chips = app.querySelector(".kind-chips");
  const warnings = app.querySelector(".filter-warnings");
  if (!(count instanceof HTMLElement) || !(csv instanceof HTMLAnchorElement) || !(search instanceof HTMLInputElement) || !(table instanceof HTMLElement) || !(tbody instanceof HTMLElement) || !(sentinel instanceof HTMLElement) || !(chips instanceof HTMLElement) || !(warnings instanceof HTMLElement)) return;

  const defaults = { sort: "value", dir: "desc" };
  const query = (extra = {}) => new URLSearchParams(Object.entries({ ...view, ...extra }).filter(([, value]) => value !== ""));
  // The address bar only carries what differs from the defaults.
  const pageUrl = (extra = {}) => `/?${new URLSearchParams([...query(extra)].filter(([key, value]) => defaults[key] !== value))}`.replace(/\?$/, "");
  let token = 0;
  let loaded = 0;
  let total = Infinity;
  let loading = false;
  let selected = -1;

  const syncView = () => {
    history.replaceState(null, "", pageUrl());
    const { cmp_gen, ...exported } = view;
    csv.href = `/api/v1/scores.csv?${new URLSearchParams(Object.entries(exported).filter(([, value]) => value !== ""))}`;
    app.querySelectorAll(".sort-button").forEach((button) => {
      const active = button.getAttribute("data-sort") === view.sort;
      button.textContent = button.textContent.replace(/ [↓↑]$/, "") + (active ? (view.dir === "desc" ? " ↓" : " ↑") : "");
      button.closest("th")?.setAttribute("aria-sort", active ? (view.dir === "desc" ? "descending" : "ascending") : "none");
    });
  };

  const nearViewport = () => sentinel.getBoundingClientRect().top < window.innerHeight + 800;

  async function loadMore() {
    if (loading || loaded >= total) return;
    loading = true;
    const mine = token;
    if (!loaded) sentinel.innerHTML = `<p class="loading">Calculating scores…</p>`;
    try {
      const page = await getJson(`/api/v1/scores?${query({ offset: String(loaded), limit: String(SCORE_PAGE_SIZE) })}`);
      if (mine !== token) return;
      total = page.total;
      loaded += page.rows.length;
      const filtered = page.total < page.database_total;
      table.classList.toggle("filtered", filtered);
      tbody.insertAdjacentHTML("beforeend", page.rows.map((row) => scoreRow(row, comparing, filtered)).join(""));
      if (page.offset === 0) {
        paintFilters(page);
        noteGeneration(page);
      }
      count.textContent = total === page.database_total ? `${total} entries` : `${total} of ${page.database_total} entries`;
      sentinel.innerHTML = loaded < total ? `<p class="loading">Loading more…</p>` : total ? "" : `<p class="empty">No matching entries.</p>`;
    } catch (error) {
      if (error?.name === "AbortError" || mine !== token) return;
      sentinel.innerHTML = `<p class="error">${escapeHtml(error?.message || error)}</p>`;
      total = loaded;
    } finally {
      if (mine === token) loading = false;
    }
    // A page shorter than the viewport leaves the sentinel in view, and
    // the observer only fires on changes.
    if (mine === token && nearViewport()) loadMore();
  }

  // Chips edit the query text itself, so the search box and URL stay
  // the single description of what's shown.
  let kinds = [];
  const statusTerms = [["is:moved", "moved"], ["is:up", "up"], ["is:down", "down"]];
  const termsOf = (q) => q.split(/\s+/).filter(Boolean);
  function paintFilters(page) {
    kinds = page.kinds;
    const present = new Set(termsOf(view.q));
    chips.innerHTML = kinds.map(({ kind, count, state }) => `<button class="kind-chip" type="button" data-kind="${escapeHtml(kind)}" data-state="${state || ""}" ${count || state ? "" : "disabled"} title="click: only ${escapeHtml(entryKinds[kind] || kind)} · shift-click: hide it">${escapeHtml(entryKinds[kind] || kind)}<small>${count}</small></button>`).join("")
      + (comparing ? `<span class="chip-divider"></span>${statusTerms.map(([term, label]) => `<button class="status-chip" type="button" data-term="${term}" data-state="${present.has(term) ? "include" : ""}">${label}</button>`).join("")}` : "");
    warnings.hidden = !page.warnings.length;
    warnings.textContent = page.warnings.join(" · ");
  }
  // The last ranking version this tab saw, so an edit made elsewhere
  // (an impact, a relation, K) can be compared against what it replaced.
  const GENERATION_KEY = "nrs.scores.generation";
  let previousGeneration = null;
  function noteGeneration(page) {
    let seen = null;
    try {
      seen = sessionStorage.getItem(GENERATION_KEY);
      sessionStorage.setItem(GENERATION_KEY, String(page.generation));
    } catch {}
    const note = app.querySelector(".compare-note");
    if (comparing) {
      if (!page.comparing && note) note.textContent = `version ${view.cmp_gen} is no longer cached -- nothing to compare against`;
      return;
    }
    if (seen !== null && seen !== String(page.generation) && note instanceof HTMLElement) {
      previousGeneration = seen;
      note.hidden = false;
    }
  }
  function setQuery(q) {
    clearTimeout(searchTimer);
    view.q = q.trim();
    // Rewriting the box mid-typing would eat a just-typed space.
    if (document.activeElement !== search) search.value = view.q;
    reload();
  }
  chips.addEventListener("click", (event) => {
    const target = event.target instanceof Element ? event.target.closest("button") : null;
    if (!(target instanceof HTMLButtonElement)) return;
    const terms = termsOf(view.q);
    const term = target.getAttribute("data-term");
    if (term) {
      setQuery((terms.includes(term) ? terms.filter((item) => item !== term) : [...terms, term]).join(" "));
      return;
    }
    const info = kinds.find((item) => item.kind === target.getAttribute("data-kind"));
    if (!info) return;
    const wanted = event.shiftKey ? "exclude" : "include";
    const kept = terms.filter((item) => !info.terms.includes(item));
    if (info.state !== wanted) kept.push(`${wanted === "exclude" ? "-" : ""}type:${info.kind}`);
    setQuery(kept.join(" "));
  });

  function reload() {
    token++;
    loading = false;
    loaded = 0;
    total = Infinity;
    selected = -1;
    tbody.innerHTML = "";
    syncView();
    loadMore();
  }

  const observer = new IntersectionObserver((entries) => {
    if (entries.some((entry) => entry.isIntersecting)) loadMore();
  }, { rootMargin: "800px 0px" });
  observer.observe(sentinel);
  pageTeardown = () => observer.disconnect();

  app.querySelector("thead")?.addEventListener("click", (event) => {
    const button = event.target instanceof Element ? event.target.closest(".sort-button") : null;
    if (!(button instanceof HTMLElement)) return;
    const column = button.getAttribute("data-sort") || "value";
    view.dir = view.sort === column && view.dir === "desc" ? "asc" : "desc";
    view.sort = column;
    reload();
  });

  search.addEventListener("input", () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => setQuery(search.value), 250);
  });
  search.addEventListener("keydown", (event) => {
    if (event.key === "Enter") setQuery(search.value);
  });

  app.querySelector("[data-clear-compare]")?.addEventListener("click", () => {
    navigate(pageUrl({ cmp_gen: "" }));
  });
  app.querySelector("[data-start-compare]")?.addEventListener("click", () => {
    if (previousGeneration !== null) navigate(pageUrl({ cmp_gen: previousGeneration }));
  });

  const select = (index) => {
    const rows = tbody.querySelectorAll("tr");
    if (!rows.length) return;
    rows[selected]?.classList.remove("selected");
    selected = Math.max(0, Math.min(rows.length - 1, index));
    rows[selected].classList.add("selected");
    rows[selected].scrollIntoView({ block: "nearest" });
  };
  pageKeys = (event, typing) => {
    if (typing) {
      if (event.key === "Escape" && event.target === search) search.blur();
      return;
    }
    if (event.key === "/") {
      event.preventDefault();
      search.focus();
      search.select();
    } else if (event.key === "j") {
      select(selected + 1);
    } else if (event.key === "k") {
      select(selected - 1);
    } else if (event.key === "Enter" && selected >= 0) {
      const id = tbody.querySelectorAll("tr")[selected]?.getAttribute("data-id");
      if (id) navigate(pathForEntry(id));
    }
  };

  syncView();
  await loadMore();
}

async function renderEntries() {
  currentEntryData = null;
  document.title = "Entries — NRS";
  app.innerHTML = `
    <h1>entries</h1>
    <p class="muted entry-subtitle">Paste a provider URL or ID to ingest it through the anime store, or pick an existing entry.</p>
    <form class="ingest-form">
      ${textField({}, "query", "anime, manga, novel, or visual novel URL / provider ID", "", "text", 'placeholder="https://anilist.co/anime/1535 or mal:5114" required')}
      <button class="button primary" type="submit">ingest</button>
      <p class="form-error" hidden></p>
    </form>
    <div class="toolbar">
      <input class="search" type="search" placeholder="filter by title…">
    </div>
    <p class="entry-count">…</p>
    <div class="entry-list"><p class="loading">Loading entries…</p></div>
  `;
  const ingest = app.querySelector(".ingest-form");
  const input = app.querySelector(".search");
  const list = app.querySelector(".entry-list");
  const count = app.querySelector(".entry-count");
  if (!(ingest instanceof HTMLFormElement) || !(input instanceof HTMLInputElement) || !(list instanceof HTMLElement) || !(count instanceof HTMLElement)) return;

  ingest.addEventListener("submit", async (event) => {
    event.preventDefault();
    const button = ingest.querySelector("button");
    const error = ingest.querySelector(".form-error");
    if (button instanceof HTMLButtonElement) button.disabled = true;
    if (error instanceof HTMLElement) error.hidden = true;
    try {
      const payload = Object.fromEntries(new FormData(ingest).entries());
      const result = await sendJson("/api/v1/entries/ingest", "POST", payload);
      navigate(pathForEntry(result.id));
    } catch (reason) {
      if (error instanceof HTMLElement) {
        error.hidden = false;
        error.textContent = reason?.message || String(reason);
      }
    } finally {
      if (button instanceof HTMLButtonElement) button.disabled = false;
    }
  });

  async function load() {
    try {
      const query = new URLSearchParams({ limit: "5000" });
      if (input.value.trim()) query.set("q", input.value.trim());
      const data = await getJson(`/api/v1/entries?${query}`);
      count.textContent = `${data.total} of ${data.database_total} entries`;
      list.innerHTML = data.entries.length
        ? data.entries.map(entryRow).join("")
        : `<p class="empty">No matching entries.</p>`;
    } catch (error) {
      renderError(error);
    }
  }
  input.addEventListener("input", () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(load, 250);
  });
  await load();
}

function entryRow(entry) {
  return `
    <a class="entry-row" href="${pathForEntry(entry.id)}" data-link>
      <span class="entry-title">${kindTag(entry.kind)}${escapeHtml(entry.title)}</span>
      <span class="entry-ref">${escapeHtml(entry.store)}:${escapeHtml(entry.store_id)}</span>
    </a>`;
}

function anchorChips(anchors) {
  if (!anchors?.length) return `<span class="dim">no anchor composition</span>`;
  return anchors
    .map(({ anchor, share }) => `<span class="chip">${escapeHtml(anchor)}<small>${Math.round(share * 100)}%</small></span>`)
    .join("");
}

function periodMarkup(period) {
  if (!period) return "";
  return `
    <div class="period">
      <div class="period-line">
        <span>${escapeHtml(period.from)}</span><span class="period-rule"></span><span>${escapeHtml(period.to)}</span>
      </div>
      <p class="period-caption">${escapeHtml(period.caption)}</p>
    </div>`;
}

function paramsMarkup(params) {
  const ignored = new Set(["kind", "source", "note", "dah_factor"]);
  const rows = Object.entries(params || {}).filter(([key]) => !ignored.has(key));
  if (!rows.length) return "";
  return `
    <dl class="detail-grid">
      ${rows.map(([key, value]) => `<dt>${escapeHtml(key)}</dt><dd class="mono">${escapeHtml(value)}</dd>`).join("")}
    </dl>`;
}

function impactCard(impact) {
  const note = impact.note || impact.source_comment;
  const source = impact.atoms?.[0]?.source_entry;
  const sourceMarkup = source
    ? `<p class="source-line">from <a href="${pathForEntry(source.id)}" data-link>${escapeHtml(source.title)}</a> ${kindTag(source.kind)}</p>`
    : "";
  return `
    <article class="impact-card" data-category="${escapeHtml(impact.category)}">
      <div class="card-head"><div class="category-badge">${escapeHtml(impact.category)}</div>
        <div class="card-copy">
          <h3 class="impact-title">${escapeHtml(impact.title)}${impact.needs_review ? '<span class="review-flag">needs review</span>' : ""}</h3>
          <p class="impact-description">${escapeHtml(impact.description)}</p>
          <div class="chips">${anchorChips(impact.anchors)}</div>
          ${sourceMarkup}
          ${periodMarkup(impact.period)}
          ${note ? `<p class="note">${escapeHtml(note)}</p>` : ""}
        </div>
        ${impact.period ? "" : `<div class="metric"><div class="metric-value">${escapeHtml(impact.metric.value)}</div><div class="metric-label">${escapeHtml(impact.metric.label)}</div>${impact.metric.progress != null ? `<div class="metric-bar"><span style="width:${Math.max(0, Math.min(1, Number(impact.metric.progress))) * 100}%"></span></div>` : ""}</div>`}
      </div>
      <details class="card-details" data-disclosure="${escapeHtml(impact.id)}"><summary>details &amp; edit</summary><div class="details-body">
        <p class="raw-id"><span class="mono">${escapeHtml(impact.id)}</span>${impact.contributor_share != null && Math.abs(impact.contributor_share - 1) > .005 ? `<span>contributor share <span class="mono">${Number(impact.contributor_share).toFixed(2)}</span></span>` : ""}</p>
        ${impact.kind ? "" : paramsMarkup(impact.params)}
        <div class="detail-actions">
          ${(impact.needs_review || (!impact.note && impact.source_comment)) ? `<button class="button primary" type="button" data-action="confirm-impact" data-id="${escapeHtml(impact.id)}">confirm</button>` : ""}
          <button hidden type="button" data-action="edit-impact" data-id="${escapeHtml(impact.id)}">edit impact</button>
        </div>
      </div></details>
    </article>`;
}

function groupedImpactCard(items) {
  const first = items[0];
  const anchors = [...new Set(items.flatMap((impact) => impact.anchors.map(({ anchor }) => anchor)))];
  const reviews = items.filter((impact) => impact.needs_review).length;
  return `<details class="impact-group" data-category="${escapeHtml(first.category)}" data-disclosure="group-${escapeHtml(first.kind)}"><summary>
    <span class="disclosure">▶</span><span class="category-badge">${escapeHtml(first.category)}</span>
    <span class="group-copy"><strong>${escapeHtml(first.title)}</strong><span class="group-description">${escapeHtml(first.description)}</span></span>
    <span class="group-count">${items.length} <small>moments</small>${reviews ? ` <em>${reviews} need review</em>` : ""}</span>
    <span class="group-anchors">${anchors.slice(0, 4).map((anchor) => `<span class="chip">${escapeHtml(anchor)}</span>`).join("")}${anchors.length > 4 ? `<span class="dim">+${anchors.length - 4}</span>` : ""}</span>
  </summary><div class="group-body">${items.map(impactCard).join("")}</div></details>`;
}

function impactSections(impacts) {
  return Object.entries(sections)
    .map(([key, [title, description]]) => {
      const items = impacts.filter((impact) => impact.section === key);
      if (!items.length) return "";
      const groups = items.reduce((result, impact) => {
        const groupKey = impact.kind || impact.id;
        (result[groupKey] ||= []).push(impact);
        return result;
      }, {});
      const content = key === "responses"
        ? Object.values(groups).map((group) => group.length > 1 ? groupedImpactCard(group) : impactCard(group[0])).join("")
        : items.map(impactCard).join("");
      return `
        <section class="subsection">
          <h3 class="subsection-title">${title}<span class="count">${items.length}</span></h3>
          <p class="section-intro">${description}</p>
          <div class="card-grid">${content}</div>
        </section>`;
    })
    .join("");
}

// Past this many anchor weights/routes an inline list is noise, so it
// moves behind a disclosure instead.
const TRANSFORM_INLINE_LIMIT = 4;

function transformParts(transform) {
  if (transform.type === "regional") return ["anchor weights", transform.weights.map(({ anchor, weight }) => `${anchor} ${Math.round(weight * 100)}%`)];
  if (transform.type === "transport") return ["anchor routes", transform.routes.map(({ from, to, weight }) => `${from}→${to} ${Math.round(weight * 100)}%`)];
  return [transform.type, []];
}

function transformList(parts) {
  return `<div class="transform-list">${parts.map((part) => `<div>${escapeHtml(part)}</div>`).join("")}</div>`;
}

function transformMarkup(transform) {
  if (!transform) return "";
  if (transform.type === "scalar") {
    return Math.abs(transform.factor - 1) < .005 ? "full weight" : `${Math.round(transform.factor * 100)}% weight`;
  }
  const [label, parts] = transformParts(transform);
  if (parts.length <= TRANSFORM_INLINE_LIMIT) return escapeHtml(parts.join(" · "));
  return `<details class="inline-disclosure"><summary>${escapeHtml(label)} · ${parts.length}</summary>${transformList(parts)}</details>`;
}

// A credit is shown as the roles expression someone actually wrote
// (`compose*0.6+arrange*0.4`), with the computed weights one click
// away for checking the math.
function relationWeightMarkup(relation) {
  if (relation.kind === "credit" && relation.params.roles) {
    const [, parts] = transformParts(relation.transform);
    const matrix = parts.length ? transformList(parts) : `<div class="transform-list">${transformMarkup(relation.transform)}</div>`;
    return `<span>${escapeHtml(relation.params.roles)}</span><details class="inline-disclosure"><summary>matrix</summary>${matrix}</details>`;
  }
  return transformMarkup(relation.transform);
}

function relationRow(relation) {
  return `<div class="relation-row">
    <a class="relation-title" href="${pathForEntry(relation.other.id)}" data-link>${escapeHtml(relation.other.title)}</a>
    <span class="relation-kind">${kindTag(relation.other.kind)}</span>
    <div class="relation-weight">${relationWeightMarkup(relation)}</div>
    <div class="relation-actions">${relation.editable ? `<button class="edit-link" type="button" data-action="edit-relation" data-id="${escapeHtml(relation.id)}">edit</button>` : ""}<button class="edit-link danger-button" type="button" data-action="delete-relation" data-id="${escapeHtml(relation.id)}">delete</button></div>
  </div>`;
}

function relationSectionsMarkup(relations) {
  return Object.entries(relationSections)
    .map(([section, [title, description, order]]) => {
      const sectionRelations = relations.filter((relation) => relation.section === section);
      if (!sectionRelations.length) return "";
      const labels = [...new Set(sectionRelations.map((relation) => relation.label))]
        .sort((a, b) => order.indexOf(a) - order.indexOf(b));
      return `
        <section class="subsection relation-section" data-section="${section}">
          <h3 class="subsection-title">${title}</h3>
          <p class="section-intro">${description}</p>
          <div class="relation-groups">
            ${labels.map((label) => {
              const rows = sectionRelations.filter((relation) => relation.label === label);
              return `<details class="relation-group" ${rows.length <= 6 ? "open" : ""} data-disclosure="relations-${escapeHtml(label)}">
                <summary>${escapeHtml(label)}<span class="count">${rows.length}</span></summary>
                ${rows.map(relationRow).join("")}
              </details>`;
            }).join("")}
          </div>
        </section>`;
    })
    .join("");
}

function fieldValue(fields, name, fallback = "") {
  return fields?.[name] ?? fallback;
}

function field(label, control, hint = "") {
  return `<label class="form-field"><span>${escapeHtml(label)}</span>${control}${hint ? `<small>${escapeHtml(hint)}</small>` : ""}</label>`;
}

function textField(fields, name, label, fallback = "", type = "text", extra = "") {
  return field(label, `<input name="${name}" type="${type}" value="${escapeHtml(fieldValue(fields, name, fallback))}" ${extra}>`);
}

function rangeField(fields, name, label, fallback = "0.5", min = "0", max = "1", step = "0.05") {
  const value = fieldValue(fields, name, fallback);
  return field(label, `<div class="range-control"><input name="${name}" type="range" min="${min}" max="${max}" step="${step}" value="${escapeHtml(value)}"><output>${escapeHtml(value)}</output><div class="range-bounds"><span>${min}</span><span>${max}</span></div></div>`);
}

function selectField(fields, name, label, choices, fallback = choices[0]?.[0] || "") {
  const value = fieldValue(fields, name, fallback);
  return field(label, `<select name="${name}">${choices.map(([key, text]) => `<option value="${escapeHtml(key)}" ${String(key) === String(value) ? "selected" : ""}>${escapeHtml(text)}</option>`).join("")}</select>`);
}

function impactKindField(value) {
  const groups = Object.entries(sections)
    .filter(([section]) => section !== "other")
    .map(([section, [title]]) => `<optgroup label="${escapeHtml(title)}">${impactKinds
      .filter(([, , kindSection]) => kindSection === section)
      .map(([key, text]) => `<option value="${escapeHtml(key)}" ${key === value ? "selected" : ""}>${escapeHtml(text)}</option>`)
      .join("")}</optgroup>`)
    .join("");
  return field("impact kind", `<select name="kind">${groups}</select>`);
}

function formatDurationInput(value) {
  const minutes = Number(value);
  if (!Number.isFinite(minutes)) return String(value ?? "");
  const seconds = Math.round(minutes * 60);
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  return h ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}` : `${m}:${String(s).padStart(2, "0")}`;
}

function durationField(fields, name, label, fallback) {
  const value = formatDurationInput(fieldValue(fields, name, fallback));
  return field(label, `<input name="${name}" type="text" value="${escapeHtml(value)}" placeholder="3:41">`, "M:SS, H:MM:SS, or numeric minutes");
}

function blendFields(fields) {
  return Array.from({ length: 4 }, (_, index) => {
    const number = index + 1;
    const anchorName = `anchor${number}`;
    const weightName = `weight${number}`;
    const selected = fieldValue(fields, anchorName);
    const weight = fieldValue(fields, weightName, "1");
    return field(`anchor ${number}`, `<div class="blend-control"><select name="${anchorName}"><option value=""></option>${anchors.map((anchor) => `<option value="${anchor}" ${anchor === selected ? "selected" : ""}>${anchor}</option>`).join("")}</select><input name="${weightName}" type="range" min="0" max="1" step="0.05" value="${escapeHtml(weight)}"><output>${escapeHtml(weight)}</output></div>`);
  }).join("");
}

function periodFields(fields, fallbackDays) {
  const fromExpression = fieldValue(fields, "period_from_expression");
  const toExpression = fieldValue(fields, "period_to_expression");
  return `
    <div class="form-field full"><span>calendar range</span><div class="date-range">
      <input name="period_from" type="date" value="${escapeHtml(fieldValue(fields, "period_from"))}"><span>→</span>
      <input name="period_to" type="date" value="${escapeHtml(fieldValue(fields, "period_to"))}">
    </div><div class="expression-row">
      <label><input name="period_from_expression" type="checkbox" value="${escapeHtml(fromExpression || "today")}" ${fromExpression ? "checked" : ""}> start follows ${escapeHtml(fromExpression || "today")}</label>
      <label><input name="period_to_expression" type="checkbox" value="${escapeHtml(toExpression || "today")}" ${toExpression ? "checked" : ""}> end follows ${escapeHtml(toExpression || "today")}</label>
    </div><small>Optional. Dynamic endpoints are resolved whenever this is saved or reimported.</small></div>
    ${textField(fields, "days", "fallback duration (days)", fallbackDays, "number", 'min="0" step="1"')}`;
}

function impactKindFields(kind, fields) {
  if (["cry", "ehi"].includes(kind)) return blendFields(fields);
  if (["nei", "aei"].includes(kind)) return `${rangeField(fields, "factor", "intensity")}${selectField(fields, "sign", "sign", [["1", "positive"], ["-1", "negative"]], "1")}${blendFields(fields)}`;
  if (kind === "epi") return `${rangeField(fields, "factor", "intensity")}${blendFields(fields)}`;
  if (kind === "pads") return `${periodFields(fields, "3")}${blendFields(fields)}`;
  if (["jumpscare", "sleepless_night", "politics", "dropped"].includes(kind)) return `<p class="form-note full">No extra parameters — this impact's score is fixed by the standard.</p>`;
  if (kind === "interest_field") return selectField(fields, "new_field", "new field?", [["true", "yes — genuinely new interest"], ["false", "no — same field as before"]], "false");
  if (kind === "additional") return rangeField(fields, "value", "signed adjustment", "0.5", "-2", "2");
  if (kind === "music") return rangeField(fields, "music_base", "music quality");
  if (kind === "waifu") return periodFields(fields, "90");
  if (kind === "music_consumed_progress") return durationField(fields, "duration_minutes", "duration", "180");
  if (kind === "meme") return `${rangeField(fields, "strength", "strength", "1", "0", "2")}${periodFields(fields, "30")}`;
  if (kind === "osu_song") return `${rangeField(fields, "personal", "personal impact")}${rangeField(fields, "community", "community impact", "0.3")}`;
  if (kind === "consumed") return `${rangeField(fields, "boredom", "boredom")}${durationField(fields, "duration_minutes", "duration", "180")}`;
  if (kind === "anime_consumed") return `${rangeField(fields, "boredom", "boredom")}${textField(fields, "episodes", "episodes", "12", "number", 'min="0" step="0.01"')}${durationField(fields, "episode_duration_minutes", "duration per episode", "20")}`;
  if (kind === "visual") return `${selectField(fields, "visual_kind", "visual kind", [["1.0", "Anime / RPG 3D game (1.0)"], ["0.8", "Animated short / MV / VN / manga (0.8)"], ["0.7", "Anime PV / animated gacha art (0.7)"], ["0.6", "Gacha card art, static (0.6)"], ["0.5", "Light novel visual / semi-animated MV (0.5)"], ["0.3", "Static music video (0.3)"], ["0.25", "Album art / anime promo art (0.25)"]], "1.0")}${rangeField(fields, "base", "quality")}${rangeField(fields, "unique", "uniqueness")}`;
  if (kind === "writing") return `${rangeField(fields, "character_complexity", "character complexity")}${rangeField(fields, "story_quality", "story quality")}${rangeField(fields, "pacing", "pacing")}${rangeField(fields, "originality", "originality")}`;
  return `<p class="error full">Unknown impact kind.</p>`;
}

function closeEditor(action) {
  const panel = action?.closest(".inline-editor-panel");
  const details = panel?.closest(".card-details");
  panel?.remove();
  if (details instanceof HTMLDetailsElement) details.open = false;
}

function bindRangeOutputs(root) {
  root.addEventListener("input", (event) => {
    if (!(event.target instanceof HTMLInputElement) || event.target.type !== "range") return;
    const output = event.target.nextElementSibling;
    if (output instanceof HTMLOutputElement) output.value = event.target.value;
  });
}

function showEditor(host, title, formMarkup, onSubmit, onDelete) {
  document.querySelectorAll(".inline-editor-panel").forEach((panel) => panel.remove());
  const editor = document.createElement("section");
  editor.className = "inline-editor-panel";
  editor.innerHTML = `<form class="editor-shell"><header><h3>${escapeHtml(title)}</h3><button class="icon-button" type="button" data-action="close-editor" aria-label="Close">×</button></header><div class="editor-form">${formMarkup}<p class="form-error" hidden></p><div class="editor-actions">${onDelete ? '<button class="danger-button" type="button" data-delete>delete</button>' : ""}<span></span><button class="button" type="button" data-action="close-editor">cancel</button><button class="button primary" type="submit">save</button></div></div></form>`;
  if (host.classList.contains("details-body")) {
    host.append(editor);
  } else {
    host.insertAdjacentElement("afterend", editor);
  }
  const shell = editor.querySelector(".editor-shell");
  const form = editor.querySelector(".editor-form");
  if (!(shell instanceof HTMLFormElement) || !(form instanceof HTMLElement)) return;
  bindRangeOutputs(form);
  shell.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (shell.dataset.busy) return;
    shell.dataset.busy = "true";
    shell.querySelectorAll("button").forEach((button) => button.disabled = true);
    const error = form.querySelector(".form-error");
    try {
      const payload = Object.fromEntries(new FormData(shell).entries());
      await onSubmit(payload);
      editor.remove();
    } catch (reason) {
      if (error instanceof HTMLElement) {
        error.hidden = false;
        error.textContent = reason?.message || String(reason);
      }
    } finally {
      delete shell.dataset.busy;
      shell.querySelectorAll("button").forEach((button) => button.disabled = false);
    }
  });
  const deleteButton = editor.querySelector("[data-delete]");
  deleteButton?.addEventListener("click", async () => {
    if (!onDelete || !confirm("Delete this permanently?")) return;
    try {
      await onDelete();
      editor.remove();
    } catch (reason) {
      const error = form.querySelector(".form-error");
      if (error instanceof HTMLElement) {
        error.hidden = false;
        error.textContent = reason?.message || String(reason);
      }
    }
  });
  if (!host.closest(".card-details")) editor.scrollIntoView({ behavior: "smooth", block: "nearest" });
  return editor;
}

// Re-fetches the entry after a write and repaints it in place -- no
// loading placeholder, scroll position and open groups kept -- so a
// save doesn't throw the page back to the top.
async function refreshEntry(entryId, options = {}) {
  if (location.pathname === pathForEntry(entryId)) await renderEntry(entryId, { ...options, quiet: true });
}

async function confirmImpact(impactId) {
  if (!currentEntryData || !impactId) return;
  const entryId = currentEntryData.entry.id;
  try {
    await sendJson(`/api/v1/entries/${encodeURIComponent(entryId)}/impacts/${encodeURIComponent(impactId)}/confirm`, "POST");
    await refreshEntry(entryId);
    showToast("Impact confirmed");
  } catch (error) {
    showToast(error?.message || String(error), "error");
  }
}

async function deleteRelation(relationId) {
  if (!currentEntryData || !relationId || !confirm("Delete this relation?")) return;
  const entryId = currentEntryData.entry.id;
  try {
    await sendJson(`/api/v1/entries/${encodeURIComponent(entryId)}/relations/${encodeURIComponent(relationId)}`, "DELETE");
    await refreshEntry(entryId);
    showToast("Relation deleted");
  } catch (error) {
    showToast(error?.message || String(error), "error");
  }
}

/** An API atom in the shape the raw-edit endpoint takes back. */
function atomPayload(atom, blend = atom.blend.map(({ anchor, weight }) => [anchor, weight])) {
  return {
    source: atom.source,
    magnitude: atom.magnitude,
    blend,
    time: atom.time.type === "moment" ? "Moment" : { Tail: { peak: atom.time.peak, lambda: atom.time.lambda, cutoff: atom.time.cutoff } },
  };
}

/**
 * One-click reclassification: `− anchor` drops an anchor from this
 * atom's blend, `+ anchor` adds it at weight 1. Each click saves
 * immediately.
 */
function blendChips(atom, index) {
  const active = atom.blend.map(({ anchor, weight }) => `<button class="blend-chip active" type="button" data-chip data-atom="${index}" data-anchor="${escapeHtml(anchor)}" data-op="remove">− ${escapeHtml(anchor)} <small>${Number(weight).toFixed(2)}</small></button>`);
  const inactive = anchors
    .filter((anchor) => !atom.blend.some((entry) => entry.anchor === anchor))
    .map((anchor) => `<button class="blend-chip" type="button" data-chip data-atom="${index}" data-anchor="${anchor}" data-op="add">+ ${anchor}</button>`);
  return `<div class="blend-chips">${[...active, ...inactive].join("")}</div>`;
}

function openRawImpactEditor(impact, action) {
  if (!currentEntryData) return;
  const entryId = currentEntryData.entry.id;
  const rawPath = `/api/v1/entries/${encodeURIComponent(entryId)}/impacts/${encodeURIComponent(impact.id)}/raw`;
  const formMarkup = `${impact.atoms.map((atom, index) => {
    const weights = Object.fromEntries(atom.blend.map(({ anchor, weight }) => [anchor, weight]));
    const choices = [...new Set([...anchors, ...Object.keys(weights)])];
    const source = atom.source_entry
      ? `<a href="${pathForEntry(atom.source_entry.id)}" data-link>${escapeHtml(atom.source_entry.title)}</a> ${kindTag(atom.source_entry.kind)}`
      : `<span class="mono">${escapeHtml(atom.source)}</span>`;
    return `<fieldset class="raw-atom"><legend>source atom ${index + 1}</legend>
      <p class="atom-source">source ${source} <span class="mono dim">· magnitude ${Number(atom.magnitude).toFixed(2)}</span></p>
      ${blendChips(atom, index)}
      <details class="raw-advanced"><summary>edit weights &amp; magnitude</summary>
        <div class="form-grid">${textField({ source: atom.source }, `source${index}`, "source entry", atom.source)}${textField({}, `magnitude${index}`, "magnitude", String(atom.magnitude), "number", 'step="any" required')}</div>
        <div class="raw-blend">${choices.map((anchor) => field(anchor, `<input type="number" name="blend${index}:${escapeHtml(anchor)}" step="any" value="${escapeHtml(weights[anchor] ?? 0)}">`)).join("")}</div>
      </details>
    </fieldset>`;
  }).join("")}${field("note", `<textarea name="note" rows="3">${escapeHtml(impact.note || impact.source_comment || "")}</textarea>`)}`;
  const editor = showEditor(action.closest(".details-body") || action, "Edit impact", formMarkup, async (payload) => {
    const atoms = impact.atoms.map((original, index) => atomPayload(
      { ...original, source: payload[`source${index}`], magnitude: Number(payload[`magnitude${index}`]) },
      Object.entries(payload).filter(([key, value]) => key.startsWith(`blend${index}:`) && Number(value) !== 0).map(([key, value]) => [key.slice(key.indexOf(":") + 1), Number(value)]),
    ));
    await sendJson(rawPath, "PUT", { note: payload.note || "", atoms });
    await refreshEntry(entryId);
    showToast("Impact saved");
  }, async () => {
    await sendJson(`/api/v1/entries/${encodeURIComponent(entryId)}/impacts/${encodeURIComponent(impact.id)}`, "DELETE");
    await refreshEntry(entryId);
    showToast("Impact deleted");
  });
  editor?.addEventListener("click", async (event) => {
    const chip = event.target instanceof Element ? event.target.closest("[data-chip]") : null;
    if (!(chip instanceof HTMLButtonElement) || editor.dataset.busy) return;
    editor.dataset.busy = "true";
    const chips = editor.querySelectorAll("[data-chip]");
    chips.forEach((button) => { if (button instanceof HTMLButtonElement) button.disabled = true; });
    const atomIndex = Number(chip.dataset.atom);
    const anchor = chip.dataset.anchor;
    const atoms = impact.atoms.map((atom, index) => {
      const blend = atom.blend.map((entry) => [entry.anchor, entry.weight]);
      if (index !== atomIndex) return atomPayload(atom, blend);
      return atomPayload(atom, chip.dataset.op === "remove" ? blend.filter(([name]) => name !== anchor) : [...blend, [anchor, 1]]);
    });
    // The note rides along so a chip click doesn't drop an unsaved
    // edit to it.
    const note = editor.querySelector('textarea[name="note"]');
    try {
      await sendJson(rawPath, "PUT", { note: note instanceof HTMLTextAreaElement ? note.value : "", atoms });
      // Keep this card's editor open so several chips can be
      // clicked in a row.
      await refreshEntry(entryId, { keepEditorFor: impact.id });
    } catch (reason) {
      const error = editor.querySelector(".form-error");
      if (error instanceof HTMLElement) {
        error.hidden = false;
        error.textContent = reason?.message || String(reason);
      }
      chips.forEach((button) => { if (button instanceof HTMLButtonElement) button.disabled = false; });
    } finally {
      delete editor.dataset.busy;
    }
  });
}

function openImpactEditor(impact = null, action) {
  if (!currentEntryData) return;
  const entryId = currentEntryData.entry.id;
  const fields = { ...(impact?.params || {}) };
  fields.kind = impact?.kind || "nei";
  fields.source ||= entryId;
  fields.dah_factor = String(impact?.contributor_share ?? 1);
  fields.note = impact?.note || impact?.source_comment || "";
  const body = `
    ${impact ? "" : `<p class="form-note editor-intro">Per DAH_standards — pick the kind, fill in its own parameters, and the score vector is computed from the spec's formula rather than picked by hand.</p>`}
    <div class="form-grid">
      ${textField(fields, "source", "source entry", entryId)}
      ${impactKindField(fields.kind)}
      <div id="impact-kind-fields" class="form-grid full">${impactKindFields(fields.kind, fields)}</div>
      ${rangeField(fields, "dah_factor", "contributor share", "1")}
      ${field("note", `<textarea name="note" rows="3">${escapeHtml(fields.note)}</textarea>`)}
    </div>`;
  const refresh = async (message) => {
    await refreshEntry(entryId);
    showToast(message);
  };
  const host = impact ? action.closest(".details-body") || action : action.closest(".section-actions") || action;
  const editor = showEditor(
    host,
    impact ? "Edit impact" : "Add impact",
    body,
    async (payload) => {
      const path = impact ? `/api/v1/entries/${encodeURIComponent(entryId)}/impacts/${encodeURIComponent(impact.id)}` : `/api/v1/entries/${encodeURIComponent(entryId)}/impacts`;
      await sendJson(path, impact ? "PUT" : "POST", payload);
      await refresh(impact ? "Impact saved" : "Impact added");
    },
    impact ? async () => {
      await sendJson(`/api/v1/entries/${encodeURIComponent(entryId)}/impacts/${encodeURIComponent(impact.id)}`, "DELETE");
      await refresh("Impact deleted");
    } : null,
  );
  const kindSelect = editor.querySelector('select[name="kind"]');
  kindSelect?.addEventListener("change", () => {
    const target = editor.querySelector("#impact-kind-fields");
    if (target instanceof HTMLElement && kindSelect instanceof HTMLSelectElement) {
      target.innerHTML = impactKindFields(kindSelect.value, {});
      bindRangeOutputs(target);
    }
  });
}

function relationKindFields(kind, fields) {
  if (kind === "contains") return rangeField(fields, "factor", "borrowed weight", "1");
  if (kind === "killed_by") return `${rangeField(fields, "potential", "lost potential")}${rangeField(fields, "effect", "actual effect")}`;
  if (kind === "remix") return `<p class="form-note full">Fixed 0.2 scalar borrow — no extra parameters.</p>`;
  if (kind === "feature_music") return `<p class="form-note full">Fixed 0.2 on the Music anchor only — no extra parameters.</p>`;
  return `<p class="error full">Unknown relation kind.</p>`;
}

function openRelationEditor(relation = null, action) {
  if (!currentEntryData) return;
  const entryId = currentEntryData.entry.id;
  const fields = { ...(relation?.params || {}) };
  fields.kind = relation?.kind || "contains";
  fields.from_entry = relation?.from || entryId;
  fields.to_entry = relation?.to || "";
  const body = `
  ${relation ? "" : `<p class="form-note editor-intro">The relation's kind already implies its transform under DAH_standards — no separate scalar/regional/transport choice to make.</p>`}
  <div class="form-grid">
    ${selectField(fields, "kind", "relation kind", relationKinds, fields.kind)}
    ${textField(fields, "from_entry", "from entry")}
    ${textField(fields, "to_entry", "to entry", "", "text", `required placeholder="other entry's raw id"`)}
    <div id="relation-kind-fields" class="form-grid full">${relationKindFields(fields.kind, fields)}</div>
    <p class="form-note full">from/to take a raw entry id (the one in this page's URL, or another entry's) — <a href="/entries" data-link>browse entries</a> to find one.</p>
  </div>`;
  const refresh = async (message) => {
    await refreshEntry(entryId);
    showToast(message);
  };
  const host = relation ? action.closest(".relation-row") || action : action.closest(".section-actions") || action;
  const editor = showEditor(
    host,
    relation ? "Edit relation" : "Add relation",
    body,
    async (payload) => {
      const path = relation ? `/api/v1/entries/${encodeURIComponent(entryId)}/relations/${encodeURIComponent(relation.id)}` : `/api/v1/entries/${encodeURIComponent(entryId)}/relations`;
      await sendJson(path, relation ? "PUT" : "POST", payload);
      await refresh(relation ? "Relation saved" : "Relation added");
    },
    relation ? async () => {
      await sendJson(`/api/v1/entries/${encodeURIComponent(entryId)}/relations/${encodeURIComponent(relation.id)}`, "DELETE");
      await refresh("Relation deleted");
    } : null,
  );
  const kindSelect = editor.querySelector('select[name="kind"]');
  kindSelect?.addEventListener("change", () => {
    const target = editor.querySelector("#relation-kind-fields");
    if (target instanceof HTMLElement && kindSelect instanceof HTMLSelectElement) {
      target.innerHTML = relationKindFields(kindSelect.value, {});
      bindRangeOutputs(target);
    }
  });
}

async function renderPalette() {
  currentEntryData = null;
  document.title = "Palette — NRS";
  app.innerHTML = `<h1>palette</h1><p class="muted">A provisional geometry prior -- not fitted psychology, and not active in the score view yet.</p><p class="loading">Loading palette…</p>`;
  try {
    const data = await getJson("/api/v1/palette");
    const cellValues = new Map(data.cells.map((cell) => [`${cell.row}\u0000${cell.column}`, cell.value]));
    app.innerHTML = `
      <h1>palette</h1>
      <p class="muted">A provisional geometry prior -- not fitted psychology, and not active in the score view yet.</p>
      <section class="page-section"><h2>Anchors</h2>
        <div class="data-table-wrap"><table class="data-table"><thead><tr><th>id</th><th>label</th><th>family</th></tr></thead><tbody>
          ${data.anchors.map((anchor) => `<tr><td class="mono brass">${escapeHtml(anchor.id)}</td><td>${escapeHtml(anchor.label)}</td><td class="muted">${escapeHtml(anchor.family || "—")}</td></tr>`).join("")}
        </tbody></table></div>
      </section>
      <section class="page-section"><h2>Feature similarity heatmap</h2><p class="muted section-copy">Brighter cells mean more double-counting. Hover a cell for the exact value.</p>
        <div class="heatmap-wrap"><table class="heatmap"><thead><tr><th></th>${data.anchors.map((anchor) => `<th title="${escapeHtml(anchor.label)}">${escapeHtml(anchor.id)}</th>`).join("")}</tr></thead><tbody>
          ${data.anchors.map((row) => `<tr><th title="${escapeHtml(row.label)}">${escapeHtml(row.id)}</th>${data.anchors.map((column) => { const value = Number(cellValues.get(`${row.id}\u0000${column.id}`) || 0); return `<td title="${escapeHtml(row.label)} × ${escapeHtml(column.label)} = ${value.toFixed(3)}" style="--cell:${Math.round((.08 + .84 * Math.max(0, Math.min(1, value))) * 100)}%">${value.toFixed(2)}</td>`; }).join("")}</tr>`).join("")}
        </tbody></table></div>
      </section>`;
  } catch (error) {
    renderError(error);
  }
}

async function renderCalibrate() {
  currentEntryData = null;
  document.title = "Calibrate — NRS";
  app.innerHTML = `<h1>calibrate</h1><div id="calibration-pair"><p class="loading">Choosing a pair…</p></div>`;
  const target = app.querySelector("#calibration-pair");
  if (!(target instanceof HTMLElement)) return;
  const paint = (pair) => {
    target.innerHTML = `<p class="calibration-prompt">Which of these do you actually think is better?</p><div class="calibration-options">
      ${[pair.left, pair.right].map((option, index) => `<button class="calibration-option" type="button" data-index="${index}"><strong>${escapeHtml(option.title)}</strong><span>current V: ${Number(option.value).toFixed(3)}</span></button>`).join("")}
    </div>`;
    target.querySelectorAll(".calibration-option").forEach((button) => button.addEventListener("click", async () => {
      const winner = Number(button.getAttribute("data-index")) === 0 ? pair.left : pair.right;
      const loser = winner === pair.left ? pair.right : pair.left;
      target.classList.add("busy");
      try {
        const next = await sendJson("/api/v1/calibration/pair", "POST", { winner: winner.id, loser: loser.id });
        paint(next);
        showToast("Preference recorded");
      } catch (error) {
        showToast(error?.message || String(error), "error");
      } finally {
        target.classList.remove("busy");
      }
    }));
  };
  try {
    paint(await getJson("/api/v1/calibration/pair"));
  } catch (error) {
    renderError(error);
  }
}

async function renderKfeat() {
  currentEntryData = null;
  document.title = "K — NRS";
  const intro = `<h1>K</h1><p class="muted">Pooling exponents. An anchor's own exponent <var>w</var> sets how repeats grow (<var>n</var> equal hits score like <var>n<sup>w</sup></var>; 1 = they simply add). A pair's exponent is where the two pool together: 1 = independent (they add), equal to both anchors' <var>w</var> = one box. Scoring merges pairs tightest-first, so only the hierarchy matters.</p>`;
  app.innerHTML = `${intro}<p class="loading">Loading K…</p>`;
  const row = (pair) => `<form class="kfeat-row"><span>${pair.a === pair.b ? escapeHtml(pair.a) : `${escapeHtml(pair.a)} ↔ ${escapeHtml(pair.b)}`}</span><input type="hidden" name="a" value="${escapeHtml(pair.a)}"><input type="hidden" name="b" value="${escapeHtml(pair.b)}"><input type="number" name="value" min="0.01" max="1" step="0.05" value="${escapeHtml(pair.value)}"><button type="submit">save</button></form>`;
  try {
    const data = await getJson("/api/v1/calibration/kfeat");
    const corrections = data.corrections.length
      ? `<h2>implied by the hierarchy</h2><p class="muted">These pairs already pool tighter than written, through a chain of tighter links. Scoring uses the tighter value; loosen a link in the chain to undo it.</p><ul class="kfeat-corrections">${data.corrections.map((c) => `<li>${escapeHtml(c.a)} ↔ ${escapeHtml(c.b)}: ${escapeHtml(c.from)} → <strong>${escapeHtml(c.to)}</strong> <span class="dim">via ${c.via.map(escapeHtml).join(" → ")}</span></li>`).join("")}</ul>`
      : "";
    app.innerHTML = `${intro}
      <h2>anchor exponents</h2><div class="kfeat-list">${data.exponents.map(row).join("")}</div>
      ${corrections}
      <h2>pair exponents</h2><div class="kfeat-list">${data.pairs.map(row).join("")}</div>`;
    app.querySelectorAll(".kfeat-row").forEach((form) => form.addEventListener("submit", async (event) => {
      event.preventDefault();
      if (!(form instanceof HTMLFormElement)) return;
      const payload = Object.fromEntries(new FormData(form).entries());
      payload.value = Number(payload.value);
      try {
        await sendJson("/api/v1/calibration/kfeat", "PUT", payload);
        showToast(payload.a === payload.b ? `${payload.a} saved` : `${payload.a} ↔ ${payload.b} saved`);
        renderKfeat();
      } catch (error) {
        showToast(error?.message || String(error), "error");
      }
    }));
  } catch (error) {
    renderError(error);
  }
}

async function renderReview() {
  currentEntryData = null;
  document.title = "Review — NRS";
  app.innerHTML = `<h1>review</h1><p class="muted">Entries with something left to confirm.</p><p class="loading">Loading review queue…</p>`;
  try {
    const data = await getJson("/api/v1/review");
    app.innerHTML = `<h1>review</h1><p class="muted">Entries with something left to confirm.</p>${data.entries.length ? `<div class="review-list">${data.entries.map((entry) => `<a href="${pathForEntry(entry.id)}" data-link>${escapeHtml(entry.title)}</a>`).join("")}</div>` : `<p class="empty">Nothing left to review.</p>`}`;
  } catch (error) {
    renderError(error);
  }
}

/**
 * `quiet` repaints in place after a write (see `refreshEntry`): open
 * groups survive, the scroll position is kept, and errors go back to
 * the caller instead of replacing the page. Impact cards' own
 * "details & edit" panels close again -- except `keepEditorFor`'s,
 * whose editor reopens.
 */
async function renderEntry(id, { quiet = false, keepEditorFor = null } = {}) {
  const disclosures = quiet ? [...app.querySelectorAll("details[data-disclosure]")] : [];
  const seenDisclosures = new Set(disclosures.map((element) => element.getAttribute("data-disclosure")));
  const openDisclosures = new Set(disclosures.filter((element) => element instanceof HTMLDetailsElement && element.open).map((element) => element.getAttribute("data-disclosure")));
  const scrollY = window.scrollY;
  if (!quiet) app.innerHTML = `<p class="loading">Loading entry…</p>`;
  try {
    const data = await getJson(`/api/v1/entries/${encodeURIComponent(id)}`);
    currentEntryData = data;
    document.title = `${data.entry.title} — NRS`;
    app.innerHTML = `
      <a class="back-link" href="/entries" data-link>← all entries</a>
      <header class="entry-header">
        <h1>${escapeHtml(data.entry.title)}</h1>
        <p class="muted mono">${kindTag(data.entry.kind)} ${escapeHtml(data.entry.store)}:${escapeHtml(data.entry.store_id)}</p>
      </header>
      <section class="page-section">
        <h2>Impacts <span class="count">${data.impacts.length}</span></h2>
        <p class="section-lead">What this entry did, why it mattered, and which feeling or craft anchor carried it.</p>
        ${data.impacts.length ? impactSections(data.impacts) : `<p class="empty">No impacts yet.</p>`}
        <div class="section-actions"><button class="button primary" type="button" data-action="add-impact">add impact</button></div>
      </section>
      <section class="page-section">
        <h2>Relations <span class="count">${data.relations.length}</span></h2>
        <p class="section-lead">Structure, lineage, credits, and influence. Open only the branch you need.</p>
        ${data.relations.length ? `<div class="relation-sections">${relationSectionsMarkup(data.relations)}</div>` : `<p class="empty">No relations yet.</p>`}
        <div class="section-actions"><button class="button primary" type="button" data-action="add-relation">add relation</button></div>
      </section>`;
    app.querySelectorAll("details[data-disclosure]").forEach((element) => {
      if (!(element instanceof HTMLDetailsElement)) return;
      const key = element.getAttribute("data-disclosure");
      if (element.classList.contains("card-details")) element.open = key === keepEditorFor;
      else if (seenDisclosures.has(key)) element.open = openDisclosures.has(key);
    });
    if (quiet) window.scrollTo(0, scrollY);
    else app.focus({ preventScroll: true });
  } catch (error) {
    if (quiet) throw error;
    renderError(error);
  }
}

function route() {
  routeVersion++;
  pageTeardown?.();
  pageTeardown = null;
  pageKeys = null;
  clearTimeout(searchTimer);
  requestController.abort();
  setActiveNav();
  const match = location.pathname.match(/^\/entries\/([^/]+)\/?$/);
  if (match) {
    renderEntry(decodeURIComponent(match[1]));
  } else if (/^\/entries\/?$/.test(location.pathname)) {
    renderEntries();
  } else if (/^\/palette\/?$/.test(location.pathname)) {
    renderPalette();
  } else if (/^\/calibrate\/?$/.test(location.pathname)) {
    renderCalibrate();
  } else if (/^\/kfeat\/?$/.test(location.pathname)) {
    renderKfeat();
  } else if (/^\/review\/?$/.test(location.pathname)) {
    renderReview();
  } else {
    renderScores();
  }
}

route();
