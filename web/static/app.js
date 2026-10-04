let state = null;
let settingsKey = "";
const cards = new Map();

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
  const response = await fetch("/api/state");
  if (!response.ok) throw new Error(response.statusText);
  state = await response.json();
  render();
}

function addLog(message) {
  const item = document.createElement("li");
  item.textContent = message;
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

function renderSettings() {
  const key = `${state.format}:${state.force}`;
  if (settingsKey === key) return;
  settingsKey = key;

  const settings = $("#settings");
  settings.innerHTML = `
    <label>
      format
      <select id="format">
        <option value="flac" ${state.format === "flac" ? "selected" : ""}>flac</option>
        <option value="wav" ${state.format === "wav" ? "selected" : ""}>wav</option>
      </select>
    </label>
    <label>
      <input type="checkbox" id="force" ${state.force ? "checked" : ""}>
      force
    </label>
    ${state.ffmpeg ? "" : '<span class="warn">ffmpeg not found; flac output is unavailable</span>'}
  `;

  $("#format").addEventListener("change", async (event) => {
    const previous = state.format;
    try {
      state = await post("/api/settings", { format: event.target.value });
      render();
    } catch (error) {
      addLog(error.message);
      event.target.value = previous;
    }
  });

  $("#force").addEventListener("change", async (event) => {
    const previous = state.force;
    try {
      state = await post("/api/settings", { force: event.target.checked });
      render();
    } catch (error) {
      addLog(error.message);
      event.target.checked = previous;
    }
  });
}

function buildCard(drive) {
  const card = document.createElement("section");
  card.className = "drive";
  card.dataset.index = drive.index;
  card.innerHTML = `
    <header>
      <h2 class="drive-label"></h2>
      <span class="status drive-status"></span>
    </header>
    <div class="path drive-path"></div>
    <div class="actions">
      <button data-action="load">Read TOC</button>
      <button data-action="rip">Rip selected</button>
      <button data-action="stop" disabled>Stop</button>
      <button data-action="eject">Eject</button>
    </div>
    <div class="toc-slot"></div>
    <div class="meta-slot"></div>
    <div class="matches-slot"></div>
    <div class="tags-slot"></div>
    <div class="rip-slot"></div>
  `;
  return card;
}

function tocTable(audioTracks, checked, active) {
  return `<table>
    <thead>
      <tr><th></th><th>#</th><th>type</th><th>start</th><th>duration</th></tr>
    </thead>
    <tbody>
      ${audioTracks
        .map(
          (track) => `
        <tr data-track-row="${track.number}">
          <td>
            <input
              type="checkbox"
              data-track="${track.number}"
              aria-label="select track ${track.number}"
              ${checked.has(track.number) ? "checked" : ""}
              ${active ? "disabled" : ""}
            >
          </td>
          <td>${track.number}</td>
          <td>${escapeHtml(track.kind)}</td>
          <td>${escapeHtml(track.startMsf || "")}</td>
          <td>${duration(track.durationSecs)}</td>
        </tr>`
        )
        .join("")}
    </tbody>
  </table>`;
}

function updateTocSlot(record, drive, active) {
  const slot = record.card.querySelector(".toc-slot");
  const tracks = drive.toc ? drive.toc.tracks : [];
  const audioTracks = tracks.filter((track) => track.kind === "audio");
  const tocKey = audioTracks.map((track) => track.number).join(",");

  if (!drive.toc) {
    record.tocKey = null;
    slot.innerHTML = '<p class="muted">No disc loaded.</p>';
    return;
  }
  if (record.tocKey !== tocKey) {
    record.tocKey = tocKey;
    record.checked = new Set(audioTracks.map((track) => track.number));
    const shown = active ? new Set(drive.rip.tracks) : record.checked;
    slot.innerHTML = tocTable(audioTracks, shown, active);
    return;
  }
  const selected = active ? new Set(drive.rip.tracks) : record.checked;
  for (const track of audioTracks) {
    const row = slot.querySelector(`[data-track-row="${track.number}"]`);
    if (!row) continue;
    const checkbox = row.querySelector('input[data-track]');
    checkbox.checked = selected.has(track.number);
    checkbox.disabled = active;
  }
}

function updateMetaSlot(card, drive) {
  card.querySelector(".meta-slot").innerHTML = `
    <div class="meta">
      <div>
        ${escapeHtml(drive.lookupStatus)}
        ${drive.coverFetching ? ' <span class="warn">(fetching cover)</span>' : ""}
      </div>
      ${drive.mcn ? `<div>MCN: ${escapeHtml(drive.mcn)}</div>` : ""}
      ${
        drive.registerUrl
          ? `<div><a href="${escapeHtml(drive.registerUrl)}" target="_blank" rel="noopener">register on MusicBrainz</a></div>`
          : ""
      }
      <div class="cover">
        <img src="${escapeHtml(drive.cover || "")}" alt="" ${
    drive.cover ? "" : "hidden"
  }>
      </div>
    </div>`;
}

function updateMatchesSlot(card, drive, active) {
  const slot = card.querySelector(".matches-slot");
  if (!drive.matches.length) {
    slot.innerHTML = "";
    return;
  }
  slot.innerHTML = `<div class="matches">
    ${drive.matches
      .map(
        (match, index) => `
      <button
        class="match ${index === drive.matchSel ? "selected" : ""}"
        data-match="${index}"
        ${active ? "disabled" : ""}
      >
        ${escapeHtml(match.artist)} — ${escapeHtml(match.album)}
        ${match.year ? ` (${escapeHtml(match.year)})` : ""}
        ${match.discPosition ? ` disc ${escapeHtml(match.discPosition)}` : ""}
      </button>`
      )
      .join("")}
  </div>`;
}

function updateTagsSlot(record, drive, active) {
  const slot = record.card.querySelector(".tags-slot");
  const meta = drive.meta;
  if (!meta) {
    if (record.formKey !== "none") {
      slot.innerHTML = '<p class="muted">No metadata.</p>';
      record.formKey = "none";
    }
    return;
  }
  const formKey = `meta:${drive.matchSel}:${meta.tracks.length}`;
  if (record.formKey !== formKey) {
    slot.innerHTML = `<form class="tags" data-tags="${drive.index}">
      <h3>Tags</h3>
      <label>Artist <input name="artist" value="${escapeHtml(meta.artist)}"></label>
      <label>Album <input name="album" value="${escapeHtml(meta.album)}"></label>
      <label>Album artist <input name="albumArtist" value="${escapeHtml(
        meta.albumArtist || ""
      )}"></label>
      <label>Year <input name="year" value="${escapeHtml(meta.year || "")}"></label>
      <table>
        <thead>
          <tr><th>#</th><th>Title</th><th>Artist</th></tr>
        </thead>
        <tbody>
          ${meta.tracks
            .map(
              (track, index) => `
            <tr>
              <td>${index + 1}</td>
              <td><input name="title-${index}" value="${escapeHtml(track.title)}"></td>
              <td><input name="artist-${index}" value="${escapeHtml(
                track.artist || ""
              )}"></td>
            </tr>`
            )
            .join("")}
        </tbody>
      </table>
      <button type="submit">Save tags</button>
    </form>`;
    record.formKey = formKey;
  }
  const submit = slot.querySelector('button[type="submit"]');
  if (submit) submit.disabled = active;
}

function updateRipSlot(record, drive) {
  const slot = record.card.querySelector(".rip-slot");
  const rip = drive.rip;
  if (!rip) {
    slot.innerHTML = "";
    record.ripRows = null;
    return;
  }
  const rows = rip.states.map((trackState) => trackState.number);
  const sameRows =
    record.ripRows &&
    record.ripRows.length === rows.length &&
    rows.every((number, i) => number === record.ripRows[i]);
  if (!sameRows) {
    record.ripRows = rows;
    slot.innerHTML = `
      <div class="rip">
        <h3>Rip</h3>
        <div class="rip-meta"></div>
        <table>
          <thead>
            <tr><th>#</th><th>state</th><th>progress</th></tr>
          </thead>
          <tbody>
            ${rip.states
              .map(
                (trackState) => `
              <tr data-state-number="${trackState.number}">
                <td>${trackState.number}</td>
                <td class="state"></td>
                <td class="prog"></td>
              </tr>`
              )
              .join("")}
          </tbody>
        </table>
        <div class="summary"></div>
      </div>`;
  }
  slot.querySelector(".rip-meta").textContent = [
    `${rip.format} → ${rip.outDir}`,
    rip.force ? "(force)" : "",
  ]
    .filter(Boolean)
    .join(" ");
  for (const trackState of rip.states) {
    const row = slot.querySelector(`[data-state-number="${trackState.number}"]`);
    if (!row) continue;
    row.querySelector(".state").textContent = trackState.status;
    row.querySelector(".prog").textContent = progress(trackState);
  }
  const summary = slot.querySelector(".summary");
  summary.textContent = rip.summary || "";
  summary.style.display = rip.summary ? "" : "none";
}

function updateCard(record, drive) {
  const card = record.card;
  const rip = drive.rip;
  const active = Boolean(rip && rip.active);

  card.querySelector(".drive-label").textContent = drive.label;
  card.querySelector(".drive-status").textContent = [
    drive.status,
    drive.disc ? ` / ${drive.disc}` : "",
    drive.loadingToc ? " / reading TOC" : "",
  ]
    .filter(Boolean)
    .join("");
  card.querySelector(".drive-path").textContent = `${drive.path}${
    drive.demo ? " (demo)" : ""
  }`;
  card.querySelector('[data-action="load"]').disabled =
    active || drive.demo || !drive.disc;
  card.querySelector('[data-action="rip"]').disabled = active || !drive.toc;
  card.querySelector('[data-action="stop"]').disabled = !active;
  card.querySelector('[data-action="eject"]').disabled = active || drive.demo;

  updateTocSlot(record, drive, active);
  updateMetaSlot(card, drive);
  updateMatchesSlot(card, drive, active);
  updateTagsSlot(record, drive, active);
  updateRipSlot(record, drive);
}

function applyRipEvent(driveId, event) {
  if (!state) return;
  const drive = state.drives.find(
    (candidate) => candidate.path === driveId || candidate.label === driveId
  );
  if (!drive || !drive.rip) return;
  const track = drive.rip.states.find((candidate) => candidate.number === event.number);
  if (!track) return;

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
      track.detail = `${event.bytes} B`;
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
        formKey: null,
        ripRows: null,
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
}

function bindDelegated() {
  const main = $("#drives");
  main.addEventListener("click", async (event) => {
    const actionButton = event.target.closest("[data-action]");
    if (actionButton) {
      const card = actionButton.closest(".drive");
      const index = Number(card.dataset.index);
      const action = actionButton.dataset.action;
      try {
        if (action === "load") {
          await post(`/api/drives/${index}/load`);
        } else if (action === "rip") {
          const tracks = [...card.querySelectorAll('input[data-track]:checked')].map(
            (input) => Number(input.dataset.track)
          );
          await post(`/api/drives/${index}/rip`, { tracks });
        } else if (action === "stop") {
          await post(`/api/drives/${index}/stop`);
        } else if (action === "eject") {
          await post(`/api/drives/${index}/eject`);
        }
      } catch (error) {
        addLog(error.message);
      }
      return;
    }
    const matchButton = event.target.closest("[data-match]");
    if (matchButton) {
      const card = matchButton.closest(".drive");
      const index = Number(card.dataset.index);
      try {
        await post(`/api/drives/${index}/match`, {
          index: Number(matchButton.dataset.match),
        });
      } catch (error) {
        addLog(error.message);
      }
    }
  });

  main.addEventListener("change", (event) => {
    const checkbox = event.target.closest('input[data-track]');
    if (!checkbox) return;
    const card = checkbox.closest(".drive");
    const record = cards.get(Number(card.dataset.index));
    if (!record || !record.checked) return;
    const number = Number(checkbox.dataset.track);
    if (checkbox.checked) record.checked.add(number);
    else record.checked.delete(number);
  });

  main.addEventListener("submit", async (event) => {
    const form = event.target.closest("form[data-tags]");
    if (!form) return;
    event.preventDefault();
    const card = form.closest(".drive");
    const index = Number(card.dataset.index);
    const data = new FormData(form);
    const trackCount = form.querySelectorAll('input[name^="title-"]').length;
    const tracks = [];
    for (let i = 0; i < trackCount; i += 1) {
      tracks.push({
        title: data.get(`title-${i}`) ?? null,
        artist: data.get(`artist-${i}`) ?? null,
      });
    }
    try {
      await post(`/api/drives/${index}/tags`, {
        artist: data.get("artist") ?? null,
        album: data.get("album") ?? null,
        albumArtist: data.get("albumArtist") ?? null,
        year: data.get("year") ?? null,
        tracks,
      });
    } catch (error) {
      addLog(error.message);
    }
  });
}

function connect() {
  const source = new EventSource("/api/events");
  source.onopen = () => {
    refresh().catch((error) => addLog(error.message));
  };
  source.onmessage = (message) => {
    const event = JSON.parse(message.data);
    if (event.type === "driveChanged") {
      if (!state) {
        refresh().catch((error) => addLog(error.message));
        return;
      }
      state.drives[event.drive.index] = event.drive;
      render();
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
