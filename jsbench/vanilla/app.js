const ADJECTIVES = [
  "pretty","large","big","small","tall","short","long","handsome","plain","quaint",
  "clean","elegant","easy","angry","crazy","helpful","mushy","odd","unsightly",
  "adorable","important","inexpensive","cheap","expensive","fancy"
];
const COLOURS = ["red","yellow","blue","green","pink","brown","purple","brown","white","black","orange"];
const NOUNS = ["table","chair","house","bbq","desk","car","pony","cookie","sandwich","burger","pizza","mouse","keyboard"];

let rows = [];
let selectedId = null;
let nextId = 1;
let seed = 1;

const tbody = document.getElementById("tbody");
const lastActionEl = document.getElementById("last-action");
const lastDurationEl = document.getElementById("last-duration");
const rowCountEl = document.getElementById("row-count");
const selectedLabelEl = document.getElementById("selected-label");

function nextRand(max) {
  seed = (seed * 1664525 + 1013904223) >>> 0;
  return (seed >>> 16) % max;
}

function nextLabel() {
  return `${ADJECTIVES[nextRand(ADJECTIVES.length)]} ${COLOURS[nextRand(COLOURS.length)]} ${NOUNS[nextRand(NOUNS.length)]}`;
}

function makeRows(count) {
  const out = new Array(count);
  for (let i = 0; i < count; i++) {
    out[i] = { id: nextId++, label: nextLabel() };
  }
  return out;
}

function findSelectedLabel() {
  if (selectedId == null) return "none";
  const row = rows.find(r => r.id === selectedId);
  return row ? row.label : "none";
}

function updateMetrics(action, durationMs) {
  lastActionEl.textContent = action;
  lastDurationEl.textContent = durationMs.toFixed(2);
  rowCountEl.textContent = String(rows.length);
  selectedLabelEl.textContent = findSelectedLabel();
}

function rowTemplate(row) {
  const tr = document.createElement("tr");
  if (selectedId === row.id) tr.className = "danger";

  const td1 = document.createElement("td");
  td1.className = "col-md-1";
  td1.textContent = String(row.id);
  tr.appendChild(td1);

  const td2 = document.createElement("td");
  td2.className = "col-md-4";
  const aSelect = document.createElement("a");
  aSelect.textContent = row.label;
  aSelect.addEventListener("click", () => selectRow(row.id));
  td2.appendChild(aSelect);
  tr.appendChild(td2);

  const td3 = document.createElement("td");
  td3.className = "col-md-1";
  const aRemove = document.createElement("a");
  const span = document.createElement("span");
  span.className = "glyphicon glyphicon-remove remove";
  span.setAttribute("aria-hidden", "true");
  aRemove.appendChild(span);
  aRemove.addEventListener("click", () => removeRow(row.id));
  td3.appendChild(aRemove);
  tr.appendChild(td3);

  const td4 = document.createElement("td");
  td4.className = "col-md-6";
  tr.appendChild(td4);

  return tr;
}

function renderAll() {
  const frag = document.createDocumentFragment();
  for (const row of rows) {
    frag.appendChild(rowTemplate(row));
  }
  tbody.textContent = "";
  tbody.appendChild(frag);
}

function measure(action, fn) {
  const start = performance.now();
  fn();
  const end = performance.now();
  updateMetrics(action, end - start);
}

function run() {
  measure("run(1000)", () => {
    rows = makeRows(1000);
    selectedId = null;
    renderAll();
  });
}

function runLots() {
  measure("runLots(10000)", () => {
    rows = makeRows(10000);
    selectedId = null;
    renderAll();
  });
}

function add() {
  measure("add(1000)", () => {
    const more = makeRows(1000);
    rows = rows.concat(more);
    const frag = document.createDocumentFragment();
    for (const row of more) frag.appendChild(rowTemplate(row));
    tbody.appendChild(frag);
  });
}

function updateEveryTenth() {
  measure("update every 10th", () => {
    for (let i = 0; i < rows.length; i += 10) rows[i].label += " !!!";
    const anchors = tbody.querySelectorAll("td.col-md-4 > a");
    for (let i = 0; i < rows.length && i < anchors.length; i += 10) {
      anchors[i].textContent = rows[i].label;
    }
  });
}

function clear() {
  measure("clear", () => {
    rows = [];
    selectedId = null;
    tbody.textContent = "";
  });
}

function swapRows() {
  measure("swapRows", () => {
    if (rows.length > 998) {
      const tmp = rows[1];
      rows[1] = rows[998];
      rows[998] = tmp;
      const rowEls = tbody.children;
      const a = rowEls[1];
      const b = rowEls[998];
      const aNext = a.nextSibling;
      const bNext = b.nextSibling;
      tbody.insertBefore(b, aNext);
      tbody.insertBefore(a, bNext);
    }
  });
}

function selectRow(id) {
  measure("select", () => {
    if (selectedId !== null) {
      const oldIndex = rows.findIndex(r => r.id === selectedId);
      if (oldIndex >= 0 && tbody.children[oldIndex]) tbody.children[oldIndex].className = "";
    }
    selectedId = id;
    const newIndex = rows.findIndex(r => r.id === id);
    if (newIndex >= 0 && tbody.children[newIndex]) tbody.children[newIndex].className = "danger";
  });
}

function removeRow(id) {
  measure("remove", () => {
    const index = rows.findIndex(r => r.id === id);
    if (index >= 0) {
      rows.splice(index, 1);
      tbody.children[index]?.remove();
    }
    if (selectedId === id) selectedId = null;
  });
}

document.getElementById("run").addEventListener("click", run);
document.getElementById("runlots").addEventListener("click", runLots);
document.getElementById("add").addEventListener("click", add);
document.getElementById("update").addEventListener("click", updateEveryTenth);
document.getElementById("clear").addEventListener("click", clear);
document.getElementById("swaprows").addEventListener("click", swapRows);

updateMetrics("boot", 0);
