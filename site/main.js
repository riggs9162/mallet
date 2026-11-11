/**
 * Mallet's website: the grid coordinate readout, the lighting comparison, download links
 * for the latest release and roadmap progress read from docs/ROADMAP.md.
 */

const REPO = "riggs9162/mallet";
const RELEASES_URL = `https://api.github.com/repos/${REPO}/releases/latest`;
const ROADMAP_URL = `https://raw.githubusercontent.com/${REPO}/main/docs/ROADMAP.md`;
const ROADMAP_PAGE = `https://github.com/${REPO}/blob/main/docs/ROADMAP.md`;

/** World units per grid cell in the hero, matching Mallet's default grid of 16. */
const UNITS_PER_CELL = 16;

/**
 * Shows the pointer's position over the hero in snapped map units, like Hammer's status bar.
 * @param {HTMLElement} area
 */
function setUpCoordinates(area) {
  const readout = area.querySelector("[data-coord-readout]");
  if (!readout) {
    return;
  }
  const cell = parseFloat(getComputedStyle(area).getPropertyValue("--cell")) || 40;
  area.addEventListener("pointermove", (event) => {
    const box = area.getBoundingClientRect();
    const x = Math.round((event.clientX - box.left - box.width / 2) / cell) * UNITS_PER_CELL;
    const y = Math.round((box.top + box.height / 2 - event.clientY) / cell) * UNITS_PER_CELL;
    readout.textContent = `x ${x}  y ${y}`;
  });
}

/**
 * Lets the range input over the lighting picture move the split between the two views.
 * @param {HTMLElement} figure
 */
function setUpComparison(figure) {
  const range = figure.querySelector("input[type=range]");
  const update = () => figure.style.setProperty("--split", `${range.value}%`);
  range.addEventListener("input", update);
  update();
}

/**
 * Whether the visitor seems to be on Linux, so its download is offered first.
 * @returns {boolean}
 */
function onLinux() {
  const platform = navigator.userAgentData?.platform || navigator.platform || navigator.userAgent;
  return /linux/i.test(platform) && !/android/i.test(navigator.userAgent);
}

/**
 * A byte count as megabytes for the download buttons.
 * @param {number} bytes
 * @returns {string}
 */
function megabytes(bytes) {
  return `${(bytes / 1048576).toFixed(1)} MB`;
}

/**
 * Points the download buttons at the latest release's files and says which version it is.
 * The buttons already lead to the releases page, so a failed request changes nothing.
 */
async function loadRelease() {
  const linux = onLinux();
  const recommended = document.querySelector(`.download[data-os="${linux ? "linux" : "windows"}"]`);
  recommended?.classList.add("recommended");
  const primary = document.querySelector("[data-primary-download]");
  if (primary) {
    primary.textContent = linux ? "Download for Linux" : "Download for Windows";
  }

  let release;
  try {
    const response = await fetch(RELEASES_URL, { headers: { Accept: "application/vnd.github+json" } });
    if (!response.ok) {
      return;
    }
    release = await response.json();
  } catch {
    return;
  }

  const assets = release.assets || [];
  for (const link of document.querySelectorAll("[data-asset]")) {
    const asset = assets.find((candidate) => candidate.name.endsWith(link.dataset.asset));
    if (!asset) {
      continue;
    }
    link.href = asset.browser_download_url;
    if (link.classList.contains("button")) {
      link.textContent = `${link.textContent.trim()} (${megabytes(asset.size)})`;
    }
  }

  const main = assets.find((asset) => asset.name.endsWith(linux ? "linux-x64.tar.gz" : "windows-x64.exe"));
  if (primary && main) {
    primary.href = main.browser_download_url;
  }

  const version = release.tag_name;
  const date = new Date(release.published_at).toLocaleDateString(undefined, { year: "numeric", month: "long", day: "numeric" });
  const line = document.querySelector("[data-release-line]");
  if (line) {
    line.textContent = `Version ${version.replace(/^v/, "")}, released ${date}. Free, for Windows and Linux.`;
  }
  const summary = document.querySelector("[data-release-summary]");
  if (summary) {
    summary.innerHTML = "";
    summary.append(`The latest release is ${version}, from ${date}. `);
    const notes = document.createElement("a");
    notes.href = release.html_url;
    notes.textContent = "Read what changed";
    summary.append(notes, ".");
  }
}

/**
 * The roadmap's milestones with their ticked and open items.
 * @param {string} markdown
 * @returns {{number: string, title: string, done: number, total: number}[]}
 */
function parseRoadmap(markdown) {
  const milestones = [];
  let current = null;
  for (const line of markdown.split(/\r?\n/)) {
    const heading = line.match(/^## Milestone (\d+): (.+)$/);
    if (heading) {
      current = { number: heading[1], title: heading[2].trim(), done: 0, total: 0 };
      milestones.push(current);
      continue;
    }
    if (line.startsWith("## ")) {
      current = null;
      continue;
    }
    const item = current && line.match(/^\s*- \[( |x|X)\]/);
    if (item) {
      current.total += 1;
      current.done += item[1] === " " ? 0 : 1;
    }
  }
  return milestones.filter((milestone) => milestone.total > 0);
}

/**
 * Fills the roadmap list with each milestone's progress.
 * @param {HTMLOListElement} list
 */
async function loadRoadmap(list) {
  let milestones;
  try {
    const response = await fetch(ROADMAP_URL);
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }
    milestones = parseRoadmap(await response.text());
  } catch {
    milestones = [];
  }

  list.innerHTML = "";
  if (milestones.length === 0) {
    const item = document.createElement("li");
    item.className = "milestone";
    const link = document.createElement("a");
    link.href = ROADMAP_PAGE;
    link.textContent = "Read the roadmap on GitHub";
    item.append(link);
    list.append(item);
    return;
  }

  for (const milestone of milestones) {
    const share = Math.round((milestone.done / milestone.total) * 100);
    const item = document.createElement("li");
    item.className = "milestone selectable";

    const name = document.createElement("span");
    name.className = "milestone-name";
    const number = document.createElement("small");
    number.textContent = `Milestone ${milestone.number}`;
    name.append(number, milestone.title.charAt(0).toUpperCase() + milestone.title.slice(1));

    const count = document.createElement("span");
    count.className = "milestone-count";
    count.textContent = `${milestone.done} of ${milestone.total} done`;

    const bar = document.createElement("span");
    bar.className = "milestone-bar";
    bar.setAttribute("role", "progressbar");
    bar.setAttribute("aria-valuenow", String(share));
    bar.setAttribute("aria-valuemin", "0");
    bar.setAttribute("aria-valuemax", "100");
    bar.setAttribute("aria-label", `Milestone ${milestone.number} progress`);
    const fill = document.createElement("span");
    fill.style.width = `${share}%`;
    bar.append(fill);

    item.append(name, count, bar);
    list.append(item);
  }
}

document.querySelectorAll("[data-coords]").forEach(setUpCoordinates);
document.querySelectorAll("[data-compare]").forEach(setUpComparison);
loadRelease();
document.querySelectorAll("[data-roadmap]").forEach(loadRoadmap);
