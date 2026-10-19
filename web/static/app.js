let state = null;
let settingsKey = "";
const cards = new Map();
const railItems = new Map();
// The drive index whose card is visible; null until the first snapshot.
let activeDrive = null;
// Card indices with a request in flight; their action buttons are disabled.
const pending = new Set();
// Logical index of the drive whose tag editor is open; null when closed.
let tagsEditorDrive = null;
// Monotonically increasing; only the newest snapshot may be applied, so an
// out-of-order response can never restore a stale view.
let snapshotSeq = 0;

const DISC_SVG = `<svg viewBox="0 0 64 64" fill="none" aria-hidden="true">
  <circle cx="32" cy="32" r="29" stroke="currentColor" stroke-width="2.5"/>
  <circle cx="32" cy="32" r="9" stroke="currentColor" stroke-width="2.5"/>
  <circle cx="32" cy="32" r="2.5" fill="currentColor"/>
  <path d="M11 41a24 24 0 0 1-1-17" stroke="currentColor" stroke-width="1.5" opacity="0.5"/>
</svg>`;

const PENCIL_SVG = `<svg viewBox="0 0 16 16" fill="none" aria-hidden="true">
  <path d="M11.33 2a1.885 1.885 0 1 1 2.667 2.667L5 13.67 1.33 14.67l1-3.67L11.33 2z" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round"/>
</svg>`;

function $(selector) {
  return document.querySelector(selector);
}

async function post(path, body) {
  const response = await fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body ?? {}),
  });
  if (!response.ok) {
    const payload = await response.json().catch(() => ({}));
    throw new Error(payload.error || response.statusText);
  }
  return response.status === 204 ? null : response.json();
}

async function refresh() {
  const seq = ++snapshotSeq;
  const response = await fetch("/api/state");
  if (!response.ok) throw new Error(response.statusText);
  applySnapshot(seq, await response.json());
}

function applySnapshot(seq, snapshot) {
  if (seq !== snapshotSeq) return;
  state = snapshot;
  render();
}

function timeStamp() {
  const d = new Date();
  const p = (n) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

function addLog(message) {
  const item = document.createElement("li");
  const time = document.createElement("span");
  time.className = "log-time";
  time.textContent = timeStamp();
  const msg = document.createElement("span");
  msg.className = "log-msg";
  msg.textContent = message;
  item.append(time, msg);
  if (/(failed|error|died|not found|denied)/i.test(message)) {
    item.classList.add("log--err");
  }
  const list = $("#log-list");
  list.prepend(item);
  while (list.children.length > 100) list.lastChild.remove();
}

function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>"']/g, (ch) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;",
  })[ch]);
}

function duration(secs) {
  if (secs == null) return "";
  const minutes = Math.floor(secs / 60);
  const seconds = secs % 60;
  return `${minutes}:${String(seconds).padStart(2, "0")}`;
}

function humanBytes(bytes) {
  if (bytes == null) return "";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KiB", "MiB", "GiB"];
  let value = bytes / 1024;
  let i = 0;
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024;
    i += 1;
  }
  return `${value.toFixed(1)} ${units[i]}`;
}

function progress(trackState) {
  if (trackState.status === "ripping" && trackState.total) {
    return `${(trackState.done / 1024 / 1024).toFixed(1)} / ${(
      trackState.total /
      1024 /
      1024
    ).toFixed(1)} MiB`;
  }
  return trackState.detail || "";
}

// Overall rip progress across the selected tracks: finished tracks count
// fully, the track being ripped counts its byte fraction, pending count 0.
function ripProgress(rip) {
  const selected = new Set(rip.tracks);
  let units = 0;
  let done = 0;
  let finished = 0;
  for (const trackState of rip.states) {
    if (!selected.has(trackState.number)) continue;
    units += 1;
    if (trackState.status === "ripping" && trackState.total) {
      done += trackState.done / trackState.total;
    } else if (trackState.status !== "pending") {
      done += 1;
      finished += 1;
    }
  }
  return { finished, units, fraction: units ? done / units : 0 };
}

function drivePill(status) {
  switch (status) {
    case "disc present":
      return "pill--ok";
    case "tray open":
      return "pill--warn";
    case "not ready":
      return "pill--warn";
    case "unreadable":
      return "pill--err";
    default:
      return "pill--muted";
  }
}

function lookupPill(status) {
  if (/…$/.test(status)) return "pill--busy";
  if (/failed/i.test(status)) return "pill--err";
  if (/no match/i.test(status)) return "pill--warn";
  if (/^found$|^loaded$|^demo disc$/i.test(status)) return "pill--ok";
  if (/match \d+ of \d+/i.test(status)) return "pill--info";
  if (/manual/i.test(status)) return "pill--warn";
  return "pill--muted";
}

function statePill(status) {
  switch (status) {
    case "ripping":
      return "busy";
    case "done":
      return "ok";
    case "failed":
      return "err";
    default:
      return "muted";
  }
}

function selectedFormats() {
  return state.formats.filter((format) => format.selected);
}

function formatSelection(format) {
  return format.selectedQuality === null || format.selectedQuality === undefined
    ? format.id
    : `${format.id}:${format.selectedQuality}`;
}

function selectedFormatSelections() {
  return selectedFormats().map(formatSelection);
}

function renderSettings() {
  const key = [
    state.force,
    state.outDir,
    state.formats
      .map(
        (format) =>
          `${format.id}:${format.selected ? formatSelection(format) : "-"}:${format.qualityOptions
            .map((option) => option.value)
            .join(",")}`
      )
      .join("|"),
  ].join(":");
  if (settingsKey === key) return;
  settingsKey = key;

  // Each format is a toggle; a selected format that has quality options
  // grows a quality selector attached to its toggle, so the quality always
  // sits right next to the format it applies to.
  const formatGroups = state.formats
    .map((format) => {
      const quality = format.qualityOptions.find(
        (option) => option.value === format.selectedQuality
      );
      const grouped = format.selected && format.qualityOptions.length > 0;
      const title = format.selected
        ? `Rip to ${format.label.toUpperCase()}${quality ? ` at ${quality.label}` : ""}`
        : `Add ${format.label.toUpperCase()} output`;
      const select = grouped
          ? `
        <select class="fmt-select" data-quality-for="${format.id}" aria-label="${escapeHtml(format.label)} quality">
          ${format.qualityOptions
            .map(
              (option) => `
          <option value="${option.value}" ${format.selectedQuality === option.value ? "selected" : ""}>
            ${escapeHtml(option.label)}
          </option>`
            )
            .join("")}
        </select>`
          : "";
      return `
      <span class="fmt">
        <button type="button" class="seg-btn ${grouped ? "seg-btn--grouped" : ""} ${format.selected ? "active" : ""}" data-format="${format.id}" title="${escapeHtml(title)}">
          ${escapeHtml(format.label)}
        </button>
        ${select}
      </span>`;
    })
    .join("");
  const settings = $("#settings");
  settings.innerHTML = `
    <span class="chip" title="output directory">
      <span>out</span>
      <span class="chip-text">${escapeHtml(state.outDir)}</span>
    </span>
    <div class="seg" role="group" aria-label="output formats">
      ${formatGroups}
    </div>
    <label class="switch" title="overwrite existing files">
      <input type="checkbox" id="force" ${state.force ? "checked" : ""}>
      <span class="switch-track" aria-hidden="true"></span>
      <span>force</span>
    </label>
  `;

  $("#force").addEventListener("change", async (event) => {
    const previous = state.force;
    const seq = ++snapshotSeq;
    try {
      applySnapshot(seq, await post("/api/settings", { force: event.target.checked }));
    } catch (error) {
      addLog(error.message);
      event.target.checked = previous;
    }
  });

  for (const select of settings.querySelectorAll("[data-quality-for]")) {
    select.addEventListener("change", async (event) => {
      const format = state.formats.find((candidate) => candidate.id === select.dataset.qualityFor);
      const previous = format?.selectedQuality;
      const formats = state.formats
        .filter((candidate) => candidate.selected)
        .map((candidate) => {
          const quality = candidate.id === select.dataset.qualityFor
            ? Number(event.target.value)
            : candidate.selectedQuality;
          return quality === null || quality === undefined
            ? candidate.id
            : `${candidate.id}:${quality}`;
        });
      const seq = ++snapshotSeq;
      try {
        applySnapshot(seq, await post("/api/settings", { formats }));
      } catch (error) {
        addLog(error.message);
        event.target.value = previous;
      }
    });
  }
}

function buildCard(drive) {
  const card = document.createElement("section");
  card.className = "drive";
  card.dataset.index = drive.index;
  card.innerHTML = `
    <header class="drive-head">
      <div class="drive-id">
        <h2 class="drive-label"></h2>
        <div class="drive-status"></div>
        <div class="path drive-path"></div>
      </div>
      <div class="actions">
        <button type="button" class="btn" data-action="load">Read TOC</button>
        <button type="button" class="btn" data-action="eject">Eject</button>
        <button type="button" class="btn btn--danger" data-action="stop" disabled>Stop</button>
        <button type="button" class="btn btn--primary" data-action="rip">Rip selected</button>
      </div>
    </header>
    <div class="drive-body">
      <aside class="disc">
        <div class="meta-slot"></div>
        <div class="matches-slot"></div>
      </aside>
      <div class="tracks">
        <div class="toc-slot"></div>
      </div>
    </div>
  `;
  return card;
}

// The track table doubles as the rip view: while a rip session exists
// (running or finished) each selected track grows a state and progress
// cell, and the overall bar + summary wrap the table, so the progress
// never leaves the list of tracks.
function tocTable(audioTracks, checked, active, titles, rip) {
  const all = audioTracks.length > 0 && audioTracks.every((t) => checked.has(t.number));
  // Only the tracks targeted by the rip get a state cell; unselected rows
  // stay plain, their checkbox already says they were not part of it.
  const ripNumbers = rip ? new Set(rip.tracks) : new Set();
  return `
    <div class="toc">
      <h3 class="section-title">Tracks</h3>
      <div class="rip-overall hidden">
        <span class="bar bar--lg"><span class="fill"></span></span>
        <span class="rip-fraction"></span>
      </div>
      <div class="rip-meta hidden"></div>
      <div class="toc-head">
        <label class="selectall">
          <input type="checkbox" data-select-all ${all ? "checked" : ""} ${active ? "disabled" : ""}>
          <span>Select all</span>
        </label>
        <span class="toc-count"></span>
        ${
          titles
            ? `
        <button
          type="button"
          class="btn btn--edit"
          data-action="edit-tags"
          ${active ? "disabled" : ""}
          title="${active ? "stop the rip before editing tags" : "open the tag editor"}"
        >
          ${PENCIL_SVG}
          <span>Edit tags</span>
        </button>`
            : ""
        }
      </div>
      <table>
        <thead>
          <tr>
            <th class="col-check"></th><th class="col-num">#</th><th>Title</th><th class="col-dur">Time</th>
            ${rip ? '<th class="col-state">state</th><th class="col-prog">progress</th>' : ""}
          </tr>
        </thead>
        <tbody>
          ${audioTracks
            .map((track, index) => {
              const title = titles && titles[index] ? titles[index].title : "";
              const inRip = ripNumbers.has(track.number);
              return `
            <tr data-track-row="${track.number}">
              <td class="col-check">
                <input
                  type="checkbox"
                  data-track="${track.number}"
                  aria-label="select track ${track.number}"
                  ${checked.has(track.number) ? "checked" : ""}
                  ${active ? "disabled" : ""}
                >
              </td>
              <td class="col-num">${track.number}</td>
              <td class="title">${
                title
                  ? escapeHtml(title)
                  : `<span class="track-fallback">Track ${track.number}</span>`
              }</td>
              <td class="col-dur">${duration(track.durationSecs)}</td>
              ${
                inRip
                  ? `
              <td class="col-state"><span class="state"></span></td>
              <td class="col-prog">
                <span class="prog-wrap" data-state-cell="${track.number}">
                  <span class="bar"><span class="fill"></span></span>
                  <span class="prog-text hidden"></span>
                </span>
              </td>`
                  : ""
              }
            </tr>`;
            })
            .join("")}
        </tbody>
      </table>
      <div class="summary hidden"></div>
    </div>`;
}

function syncTocSlot(slot, audioTracks, selected, active) {
  for (const track of audioTracks) {
    const row = slot.querySelector(`[data-track-row="${track.number}"]`);
    if (!row) continue;
    const checkbox = row.querySelector('input[data-track]');
    checkbox.checked = selected.has(track.number);
    checkbox.disabled = active;
  }
  const selectedCount = audioTracks.filter((track) => selected.has(track.number)).length;
  const selectAll = slot.querySelector("[data-select-all]");
  if (selectAll) {
    selectAll.checked = selectedCount === audioTracks.length && selectedCount > 0;
    selectAll.indeterminate = selectedCount > 0 && selectedCount < audioTracks.length;
    selectAll.disabled = active;
  }
  const count = slot.querySelector(".toc-count");
  if (count) {
    count.textContent = `${selectedCount} of ${audioTracks.length} selected`;
  }
  // The render key does not include the rip's active flag, so the button's
  // disabled state is kept in step here rather than at render time.
  const editTags = slot.querySelector('[data-action="edit-tags"]');
  if (editTags) {
    editTags.disabled = active;
    editTags.title = active
      ? "stop the rip before editing tags"
      : "open the tag editor";
  }
}

function updateTocSlot(record, drive, active) {
  const slot = record.card.querySelector(".toc-slot");
  const tracks = drive.toc ? drive.toc.tracks : [];
  const audioTracks = tracks.filter((track) => track.kind === "audio");
  const titles = drive.meta ? drive.meta.tracks : null;
  const rip = drive.rip;
  const numbersKey = audioTracks.map((track) => track.number).join(",");
  const titleKey = titles ? titles.map((track) => track.title).join("|") : "";
  // Which rows carry rip cells changes the table shape, so the selected
  // set is part of the render key; per-track state updates are incremental.
  const ripKey = rip ? rip.tracks.join(",") : "none";

  if (!drive.toc) {
    record.tocKey = null;
    record.tocRenderKey = null;
    slot.innerHTML = '<div class="slot-empty">No disc loaded.</div>';
    return;
  }
  if (record.tocKey !== numbersKey) {
    record.tocKey = numbersKey;
    record.checked = new Set(audioTracks.map((track) => track.number));
    record.tocRenderKey = null;
  }
  const selected = active ? new Set(rip.tracks) : record.checked;
  const renderKey = `${numbersKey}|${titleKey}|${ripKey}`;
  if (record.tocRenderKey !== renderKey) {
    record.tocRenderKey = renderKey;
    slot.innerHTML = tocTable(audioTracks, selected, active, titles, rip);
  }
  syncTocSlot(slot, audioTracks, selected, active);
  syncRipSlot(slot, rip);
}

// Overlay of controls revealed on hover/focus of the cover: replace the
// matched art with a pasted image, a file, or a URL, and reset back to the
// matched art. Only once the disc has a TOC; disabled while a rip runs.
function coverControls(drive, active) {
  if (!drive.toc) return "";
  const disabled = active ? "disabled" : "";
  const stopTitle = "stop the rip before changing the cover";
  return `
    <span class="cover-custom-pill ${drive.coverCustom ? "" : "hidden"}">custom</span>
    <div class="cover-controls">
      <button type="button" class="btn btn--sm" data-action="set-cover" ${disabled} title="${active ? stopTitle : "paste an image or pick a file"}">Set cover</button>
      <button type="button" class="btn btn--sm" data-action="cover-url" ${disabled} title="${active ? stopTitle : "fetch the cover from a URL"}">From URL</button>
      ${
    drive.coverCustom
      ? `<button type="button" class="btn btn--sm" data-action="reset-cover" ${disabled} title="${active ? stopTitle : "restore the matched cover"}">Reset</button>`
      : ""
  }
    </div>`;
}

function readAsDataUrl(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result);
    reader.onerror = () => reject(new Error("could not read that image"));
    reader.readAsDataURL(file);
  });
}

async function sendCoverFile(index, file) {
  if (pending.has(index)) return;
  pending.add(index);
  try {
    const data = await readAsDataUrl(file);
    const seq = ++snapshotSeq;
    applySnapshot(seq, await post(`/api/drives/${index}/cover`, { data }));
  } catch (error) {
    addLog(error.message);
  } finally {
    pending.delete(index);
    render();
  }
}

async function coverFromUrl(index) {
  if (pending.has(index)) return;
  const url = (window.prompt("Cover image URL") || "").trim();
  if (!url) return;
  pending.add(index);
  try {
    const seq = ++snapshotSeq;
    applySnapshot(seq, await post(`/api/drives/${index}/cover`, { url }));
  } catch (error) {
    addLog(error.message);
  } finally {
    pending.delete(index);
    render();
  }
}

function pickCoverFile(index) {
  const input = document.createElement("input");
  input.type = "file";
  input.accept = "image/*";
  input.addEventListener("change", () => {
    const file = input.files && input.files[0];
    if (file) sendCoverFile(index, file);
  });
  input.click();
}

function updateMetaSlot(record, drive) {
  // Rebuilding here re-assigns the cover's data URL, making the browser
  // re-decode it; skip the rebuild when none of the inputs changed.
  const meta = drive.meta;
  const active = Boolean(drive.rip && drive.rip.active);
  const metaKey = [
    drive.lookupStatus,
    drive.mcn || "",
    drive.registerUrl || "",
    drive.coverFetching,
    drive.coverCustom,
    active,
    Boolean(drive.toc),
    meta ? `${meta.artist}\u0000${meta.album}\u0000${meta.year || ""}` : "",
  ].join("\u0000");
  if (metaKey === record.metaKey && drive.cover === record.metaCover) return;
  record.metaKey = metaKey;
  record.metaCover = drive.cover;
  const album = meta ? meta.album : "";
  const artist = meta ? meta.artist : "";
  const year = meta ? meta.year : null;
  record.card.querySelector(".meta-slot").innerHTML = `
    <div class="cover" tabindex="0" aria-label="Album cover: paste an image, or use the buttons to replace it">
      <img src="${escapeHtml(drive.cover || "")}" alt="cover art" ${
    drive.cover ? "" : "hidden"
  }>
      <div class="cover-fallback" ${drive.cover ? "hidden" : ""}>${DISC_SVG}</div>
      ${coverControls(drive, active)}
    </div>
    <div class="disc-title">
      <h3 class="disc-album">${
    album
      ? escapeHtml(album)
      : drive.toc
        ? "Unidentified disc"
        : "No disc loaded"
  }</h3>
      <div class="disc-artist">${
    artist
      ? `${escapeHtml(artist)}${year ? ` <span class="disc-year">· ${escapeHtml(year)}</span>` : ""}`
      : ""
  }</div>
    </div>
    <div class="disc-lookup">
      <span class="pill ${lookupPill(drive.lookupStatus)}"><span class="pill-dot"></span>${escapeHtml(
        drive.lookupStatus || "idle"
      )}</span>
      ${
    drive.coverFetching
      ? '<span class="pill pill--muted"><span class="pill-dot"></span>fetching cover</span>'
      : ""
  }
    </div>
    <div class="disc-extra">
      ${
    drive.mcn
      ? `<div class="mcn"><span>MCN</span><code>${escapeHtml(drive.mcn)}</code></div>`
      : ""
  }
      ${
    drive.registerUrl
      ? `<a class="register" href="${escapeHtml(drive.registerUrl)}" target="_blank" rel="noopener">register on MusicBrainz ↗</a>`
      : ""
  }
    </div>`;
}

function updateMatchesSlot(card, drive, active) {
  const slot = card.querySelector(".matches-slot");
  if (!drive.matches.length) {
    slot.innerHTML = "";
    return;
  }
  slot.innerHTML = `<div class="matches">
    ${drive.matches.length > 1 ? '<h3 class="section-title">Matches</h3>' : ""}
    ${drive.matches
      .map(
        (match, index) => `
      <button
        type="button"
        class="match ${index === drive.matchSel ? "selected" : ""}"
        data-match="${index}"
        ${active ? "disabled" : ""}
      >
        <span class="match-radio" aria-hidden="true"></span>
        <span class="match-text">
          <span class="match-line1">${escapeHtml(match.artist)} — ${escapeHtml(match.album)}</span>
          <span class="match-line2">${
            [
              match.year ? escapeHtml(match.year) : "",
              match.discPosition ? `disc ${escapeHtml(match.discPosition)}` : "",
            ]
              .filter(Boolean)
              .join(" · ")
          }</span>
        </span>
      </button>`
      )
      .join("")}
  </div>`;
}

// Collapsed view of the tags section: just a bar with an edit button.
// The full editor lives in a modal so a long track list never pushes the
// rest of the card down.
function tagsFormHtml(drive) {
  const meta = drive.meta;
  return `
    <div class="tags-grid">
      <label>Artist <input name="artist" value="${escapeHtml(meta.artist)}"></label>
      <label>Album <input name="album" value="${escapeHtml(meta.album)}"></label>
      <label>Album artist <input name="albumArtist" value="${escapeHtml(
        meta.albumArtist || ""
      )}"></label>
      <label>Year <input name="year" value="${escapeHtml(meta.year || "")}"></label>
    </div>
    <table>
      <thead>
        <tr><th class="col-num">#</th><th>Title</th><th>Track artist</th></tr>
      </thead>
      <tbody>
        ${meta.tracks
          .map(
            (track, index) => `
          <tr>
            <td class="col-num">${index + 1}</td>
            <td><input name="title-${index}" value="${escapeHtml(track.title)}"></td>
            <td><input name="artist-${index}" value="${escapeHtml(
              track.artist || ""
            )}"></td>
          </tr>`
          )
          .join("")}
      </tbody>
    </table>`;
}

function openTagsEditor(index) {
  if (tagsEditorDrive !== null) return;
  const drive = state.drives.find((candidate) => candidate.index === index);
  if (!drive || !drive.meta) return;
  tagsEditorDrive = index;
  const form = $("#tags-form");
  form.dataset.tags = String(index);
  form.innerHTML = tagsFormHtml(drive);
  $("#tags-modal-title").textContent = `Edit tags — ${drive.meta.artist} — ${drive.meta.album}`;
  $("#tags-modal").classList.remove("hidden");
  form.querySelector("input")?.focus();
}

function closeTagsEditor() {
  if (tagsEditorDrive === null) return;
  const index = tagsEditorDrive;
  tagsEditorDrive = null;
  $("#tags-modal").classList.add("hidden");
  $("#tags-form").innerHTML = "";
  document
    .querySelector(`.drive[data-index="${index}"] [data-action="edit-tags"]`)
    ?.focus();
}

// Incrementally paints the rip state onto the track table: the overall
// bar, the per-track state pill + progress bar in the row cells, and the
// final summary. Cells only exist for tracks in the current rip session.
function syncRipSlot(slot, rip) {
  const overall = slot.querySelector(".rip-overall");
  if (!overall) return;
  const meta = slot.querySelector(".rip-meta");
  const summary = slot.querySelector(".summary");
  if (!rip) {
    overall.classList.add("hidden");
    meta.classList.add("hidden");
    summary.classList.add("hidden");
    summary.textContent = "";
    return;
  }
  overall.classList.remove("hidden");
  meta.classList.remove("hidden");
  meta.textContent = [
    `${(rip.formats ?? []).join(", ")} → ${rip.outDir}`,
    rip.force ? "(force)" : "",
  ]
    .filter(Boolean)
    .join(" ");
  const { finished, units, fraction } = ripProgress(rip);
  overall.querySelector(".fill").style.width = `${Math.round(fraction * 100)}%`;
  overall.querySelector(".rip-fraction").textContent = `${finished} of ${units} tracks`;
  for (const trackState of rip.states) {
    const wrap = slot.querySelector(`[data-state-cell="${trackState.number}"]`);
    if (!wrap) continue;
    const row = wrap.closest("tr");
    row.classList.toggle("row--ripping", trackState.status === "ripping");
    row.querySelector(".state").innerHTML = `
      <span class="pill pill--sm pill--${statePill(trackState.status)}"><span class="pill-dot"></span>${trackState.status}</span>`;
    let width = 0;
    let barClass = "";
    if (trackState.status === "done") {
      width = 100;
      barClass = "bar--done";
    } else if (trackState.status === "failed") {
      barClass = "bar--fail";
    }
    if (trackState.total) {
      width = Math.max(width, Math.round((trackState.done / trackState.total) * 100));
    }
    const bar = wrap.querySelector(".bar");
    bar.className = `bar${barClass ? ` ${barClass}` : ""}`;
    bar.querySelector(".fill").style.width = `${width}%`;
    const text = progress(trackState);
    const progText = wrap.querySelector(".prog-text");
    progText.textContent = text;
    progText.classList.toggle("hidden", !text);
    progText.classList.toggle("prog-text--err", trackState.status === "failed");
  }
  summary.textContent = rip.summary || "";
  summary.classList.toggle("hidden", !rip.summary);
}

function updateCard(record, drive) {
  const card = record.card;
  const rip = drive.rip;
  const active = Boolean(rip && rip.active);

  card.querySelector(".drive-label").textContent = drive.label;
  card.querySelector(".drive-status").innerHTML = [
    `<span class="pill ${drivePill(drive.status)}"><span class="pill-dot"></span>${escapeHtml(
      drive.status || "unknown"
    )}</span>`,
    drive.disc
      ? `<span class="pill pill--muted">${escapeHtml(drive.disc)} disc</span>`
      : "",
    drive.demo ? '<span class="pill pill--violet">demo</span>' : "",
  ]
    .filter(Boolean)
    .join("");
  card.querySelector(".drive-path").textContent = drive.path;
  const busy = active || pending.has(drive.index);
  card.querySelector('[data-action="load"]').disabled =
    busy || drive.demo || !drive.disc;
  card.querySelector('[data-action="rip"]').disabled = busy || !drive.toc;
  card.querySelector('[data-action="stop"]').disabled = !active;
  card.querySelector('[data-action="eject"]').disabled = busy || drive.demo;

  updateTocSlot(record, drive, active);
  updateMetaSlot(record, drive);
  updateMatchesSlot(card, drive, active);
}

// What a rail row says about its drive: the status dot, the one-line
// description of what is in the drive, and a live activity indicator.
function railInfo(drive) {
  const rip = drive.rip;
  if (rip && rip.active) {
    const { finished, units, fraction } = ripProgress(rip);
    return {
      dot: "busy",
      sub: `ripping ${finished}/${units}`,
      ind: `<span class="bar"><span class="fill" style="width:${Math.round(
        fraction * 100
      )}%"></span></span><span class="rail-count">${finished}/${units}</span>`,
    };
  }
  if (drive.loadingToc || drive.coverFetching || /…$/.test(drive.lookupStatus)) {
    return { dot: "busy", sub: drive.status || "working", ind: '<span class="spinner"></span>' };
  }
  if (rip) {
    const failed = rip.states.some((track) => track.status === "failed");
    return {
      dot: failed ? "err" : "ok",
      sub: failed ? "rip failed" : "rip done",
      ind: `<span class="dot ${failed ? "dot--failed" : "dot--done"}"></span>`,
    };
  }
  const dot =
    drive.status === "disc present"
      ? "ok"
      : drive.status === "tray open" || drive.status === "not ready"
        ? "warn"
        : drive.status === "unreadable"
          ? "err"
          : "muted";
  return {
    dot,
    sub:
      drive.status === "disc present"
        ? drive.meta?.album || (drive.toc ? "disc loaded" : `${drive.disc || "disc"} disc`)
        : drive.status || "unknown",
    ind: "",
  };
}

function updateRailItem(item, drive) {
  item.querySelector(".rail-label").textContent = drive.label;
  const info = railInfo(drive);
  item.querySelector(".rail-dot").className = `rail-dot rail-dot--${info.dot}`;
  item.querySelector(".rail-sub").textContent = info.sub;
  item.querySelector(".rail-ind").innerHTML = info.ind;
  item.classList.toggle("rail-item--ripping", Boolean(drive.rip && drive.rip.active));
  item.title = [drive.status, drive.disc ? `${drive.disc} disc` : "", drive.meta?.album]
    .filter(Boolean)
    .join(" / ");
}

function applyActiveRail() {
  for (const [index, item] of railItems) {
    const selected = index === activeDrive;
    item.classList.toggle("active", selected);
    item.setAttribute("aria-selected", selected ? "true" : "false");
    item.tabIndex = selected ? 0 : -1;
  }
}

function setActiveDrive(index) {
  if (index === activeDrive) return;
  activeDrive = index;
  showActiveCard();
  applyActiveRail();
}

// Arrow keys, j/k, Home and End move between drives, following the ARIA
// tablist pattern (vertical orientation).
function onRailKeydown(event) {
  const order = state.drives.map((drive) => drive.index);
  if (!order.length) return;
  const current = order.indexOf(activeDrive);
  let next;
  if (event.key === "ArrowUp" || event.key === "k") {
    next = order[(current - 1 + order.length) % order.length];
  } else if (event.key === "ArrowDown" || event.key === "j") {
    next = order[(current + 1) % order.length];
  } else if (event.key === "Home") {
    next = order[0];
  } else if (event.key === "End") {
    next = order[order.length - 1];
  } else {
    return;
  }
  event.preventDefault();
  setActiveDrive(next);
  railItems.get(next)?.focus({ preventScroll: true });
}

function renderRail() {
  const rail = $("#rail");
  rail.classList.toggle("hidden", !state.drives.length);
  const seen = new Set();
  state.drives.forEach((drive) => {
    seen.add(drive.index);
    let item = railItems.get(drive.index);
    if (!item) {
      item = document.createElement("button");
      item.type = "button";
      item.className = "rail-item";
      item.dataset.index = drive.index;
      item.setAttribute("role", "tab");
      item.innerHTML = `<span class="rail-dot" aria-hidden="true"></span>
        <span class="rail-text">
          <span class="rail-label"></span>
          <span class="rail-sub"></span>
        </span>
        <span class="rail-ind" aria-hidden="true"></span>`;
      railItems.set(drive.index, item);
      rail.appendChild(item);
    }
    updateRailItem(item, drive);
  });
  for (const [index, item] of railItems) {
    if (!seen.has(index)) {
      item.remove();
      railItems.delete(index);
    }
  }
  if (!seen.has(activeDrive)) activeDrive = null;
  if (activeDrive === null && state.drives.length) {
    activeDrive = state.drives[0].index;
  }
  applyActiveRail();
}

function showActiveCard() {
  for (const [index, record] of cards) {
    record.card.classList.toggle("hidden", index !== activeDrive);
  }
}

function applyRipEvent(driveId, event) {
  if (!state) return;
  const drive = state.drives.find(
    (candidate) => candidate.path === driveId || candidate.label === driveId
  );
  if (!drive || !drive.rip) return;
  // Per-track events carry the track number; the terminal "finished" event
  // does not, so only require the lookup for the former.
  const track =
    event.number != null
      ? drive.rip.states.find((candidate) => candidate.number === event.number)
      : null;
  if (event.number != null && !track) return;

  switch (event.event) {
    case "trackStarted":
      track.status = "ripping";
      track.detail = null;
      break;
    case "progress":
      track.status = "ripping";
      track.done = event.bytesDone;
      track.total = event.bytesTotal;
      break;
    case "trackDone":
      track.status = "done";
      track.detail = humanBytes(event.bytes);
      break;
    case "trackFailed":
      track.status = "failed";
      track.detail = event.error;
      break;
    case "trackSkipped":
      track.status = "skipped";
      track.detail = event.reason;
      break;
    case "finished": {
      drive.rip.active = false;
      const succeeded = event.total - event.failed;
      drive.rip.summary = event.stopped
        ? `stopped after ${succeeded} of ${event.total}`
        : `finished: ${succeeded} of ${event.total}${
            event.failed ? `, ${event.failed} failed` : ""
          }`;
      break;
    }
    default:
      break;
  }
}

function render() {
  if (!state) return;
  renderSettings();
  renderRail();
  $("#empty").classList.toggle("hidden", state.drives.length > 0);
  const main = $("#drives");
  const seen = new Set();
  state.drives.forEach((drive) => {
    seen.add(drive.index);
    let record = cards.get(drive.index);
    if (!record) {
      record = {
        card: buildCard(drive),
        checked: new Set(),
        tocKey: null,
        tocRenderKey: null,
        metaKey: null,
        metaCover: null,
      };
      cards.set(drive.index, record);
      main.appendChild(record.card);
    }
    updateCard(record, drive);
  });
  for (const [index, record] of cards) {
    if (!seen.has(index)) {
      record.card.remove();
      cards.delete(index);
    }
  }
  if (tagsEditorDrive !== null) {
    const stillThere = state.drives.some((drive) => drive.index === tagsEditorDrive);
    if (!stillThere) {
      closeTagsEditor();
    } else {
      const save = $("#tags-save");
      if (save) save.disabled = pending.has(tagsEditorDrive);
    }
  }
  showActiveCard();
}

function bindDelegated() {
  $("#rail").addEventListener("click", (event) => {
    const item = event.target.closest(".rail-item");
    if (!item) return;
    setActiveDrive(Number(item.dataset.index));
  });
  $("#rail").addEventListener("keydown", onRailKeydown);
  const settings = $("#settings");
  settings.addEventListener("click", async (event) => {
    const button = event.target.closest(".seg-btn");
    if (!button) return;
    const format = state.formats.find((candidate) => candidate.id === button.dataset.format);
    if (!format) return;
    if (format.selected && selectedFormats().length === 1) {
      addLog("keep at least one output format selected");
      return;
    }
    const formats = state.formats
      .filter((candidate) => (candidate.id === format.id ? !format.selected : candidate.selected))
      .map(formatSelection);
    const seq = ++snapshotSeq;
    try {
      applySnapshot(seq, await post("/api/settings", { formats }));
    } catch (error) {
      addLog(error.message);
    }
  });
  const main = $("#drives");
  main.addEventListener("click", async (event) => {
    const actionButton = event.target.closest("[data-action]");
    if (actionButton) {
      const card = actionButton.closest(".drive");
      const index = Number(card.dataset.index);
      if (pending.has(index)) return;
      const action = actionButton.dataset.action;
      if (action === "edit-tags") {
        openTagsEditor(index);
        return;
      }
      if (action === "set-cover") {
        pickCoverFile(index);
        return;
      }
      if (action === "cover-url") {
        coverFromUrl(index);
        return;
      }
      pending.add(index);
      try {
        if (action === "load") {
          const seq = ++snapshotSeq;
          applySnapshot(seq, await post(`/api/drives/${index}/load`));
        } else if (action === "rip") {
          const tracks = [...card.querySelectorAll('input[data-track]:checked')].map(
            (input) => Number(input.dataset.track)
          );
          if (!tracks.length) {
            addLog("select at least one track to rip");
            return;
          }
          const seq = ++snapshotSeq;
          applySnapshot(seq, await post(`/api/drives/${index}/rip`, { tracks }));
        } else if (action === "stop") {
          const seq = ++snapshotSeq;
          applySnapshot(seq, await post(`/api/drives/${index}/stop`));
        } else if (action === "eject") {
          const seq = ++snapshotSeq;
          applySnapshot(seq, await post(`/api/drives/${index}/eject`));
        } else if (action === "reset-cover") {
          const seq = ++snapshotSeq;
          applySnapshot(seq, await post(`/api/drives/${index}/cover/reset`));
        }
      } catch (error) {
        addLog(error.message);
      } finally {
        pending.delete(index);
        render();
      }
      return;
    }
    const matchButton = event.target.closest("[data-match]");
    if (matchButton) {
      const card = matchButton.closest(".drive");
      const index = Number(card.dataset.index);
      if (pending.has(index)) return;
      pending.add(index);
      try {
        const seq = ++snapshotSeq;
        applySnapshot(
          seq,
          await post(`/api/drives/${index}/match`, {
            index: Number(matchButton.dataset.match),
          })
        );
      } catch (error) {
        addLog(error.message);
      } finally {
        pending.delete(index);
        render();
      }
    }
  });

  // Pasting an image anywhere in a drive card sets it as the cover. The tags
  // editor lives outside the cards, so those pastes are never hijacked.
  main.addEventListener("paste", (event) => {
    const card = event.target.closest(".drive");
    if (!card) return;
    const target = event.target;
    if (
      target &&
      (target.tagName === "INPUT" ||
        target.tagName === "TEXTAREA" ||
        target.isContentEditable)
    ) {
      return;
    }
    const index = Number(card.dataset.index);
    const drive = state.drives.find((candidate) => candidate.index === index);
    if (!drive || !drive.toc) return;
    if (pending.has(index)) return;
    if (drive.rip && drive.rip.active) return;
    const items = event.clipboardData && event.clipboardData.items;
    if (!items) return;
    for (const item of items) {
      if (item.kind === "file" && item.type.startsWith("image/")) {
        const file = item.getAsFile();
        if (file) {
          event.preventDefault();
          sendCoverFile(index, file);
          return;
        }
      }
    }
  });

  main.addEventListener("change", (event) => {
    const checkbox = event.target.closest("input[data-track]");
    if (checkbox) {
      const card = checkbox.closest(".drive");
      const record = cards.get(Number(card.dataset.index));
      if (!record || !record.checked) return;
      const number = Number(checkbox.dataset.track);
      if (checkbox.checked) record.checked.add(number);
      else record.checked.delete(number);
      syncTocSlot(
        card.querySelector(".toc-slot"),
        [...card.querySelectorAll("input[data-track]")].map((box) => ({
          number: Number(box.dataset.track),
        })),
        record.checked,
        false
      );
      return;
    }
    const selectAll = event.target.closest("[data-select-all]");
    if (selectAll) {
      const card = selectAll.closest(".drive");
      const record = cards.get(Number(card.dataset.index));
      if (!record) return;
      const boxes = card.querySelectorAll("input[data-track]");
      for (const box of boxes) {
        box.checked = selectAll.checked;
        const number = Number(box.dataset.track);
        if (selectAll.checked) record.checked.add(number);
        else record.checked.delete(number);
      }
      syncTocSlot(
        card.querySelector(".toc-slot"),
        [...boxes].map((box) => ({ number: Number(box.dataset.track) })),
        record.checked,
        false
      );
    }
  });

  const modal = $("#tags-modal");
  modal.addEventListener("click", (event) => {
    if (event.target.closest("[data-close-modal]") || event.target === modal) {
      closeTagsEditor();
    }
  });
  modal.addEventListener("submit", async (event) => {
    const form = event.target.closest("form[data-tags]");
    if (!form) return;
    event.preventDefault();
    const index = Number(form.dataset.tags);
    if (tagsEditorDrive !== index || pending.has(index)) return;
    const data = new FormData(form);
    const trackCount = form.querySelectorAll('input[name^="title-"]').length;
    const tracks = [];
    for (let i = 0; i < trackCount; i += 1) {
      tracks.push({
        title: data.get(`title-${i}`),
        artist: data.get(`artist-${i}`),
      });
    }
    pending.add(index);
    try {
      const seq = ++snapshotSeq;
      applySnapshot(
        seq,
        await post(`/api/drives/${index}/tags`, {
          artist: data.get("artist"),
          album: data.get("album"),
          albumArtist: data.get("albumArtist"),
          year: data.get("year"),
          tracks,
        })
      );
      closeTagsEditor();
    } catch (error) {
      addLog(error.message);
    } finally {
      pending.delete(index);
      render();
    }
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") closeTagsEditor();
  });
}

function setConn(on) {
  const conn = $("#conn");
  conn.classList.toggle("on", on);
  conn.classList.toggle("off", !on);
  conn.querySelector(".conn-text").textContent = on ? "live" : "reconnecting";
}

function connect() {
  const source = new EventSource("/api/events");
  source.onopen = () => {
    setConn(true);
    refresh().catch((error) => addLog(error.message));
  };
  source.onerror = () => {
    setConn(false);
  };
  source.onmessage = (message) => {
    let event;
    try {
      event = JSON.parse(message.data);
    } catch {
      return;
    }
    if (event.type === "driveChanged") {
      if (!state) {
        refresh().catch((error) => addLog(error.message));
        return;
      }
      // Locate the drive by its logical index (not array position) so a
      // re- or removal of devices can't shift the wrong card.
      const slot = state.drives.findIndex((drive) => drive.index === event.drive.index);
      if (slot === -1) {
        refresh().catch((error) => addLog(error.message));
      } else {
        state.drives[slot] = event.drive;
        render();
      }
    } else if (event.type === "rip") {
      applyRipEvent(event.drive, event.event);
      render();
    } else if (event.type === "log") {
      addLog(event.msg);
    } else if (event.type === "resync") {
      refresh().catch((error) => addLog(error.message));
    }
  };
}

(async function init() {
  await refresh().catch((error) => addLog(error.message));
  bindDelegated();
  connect();
})();
