import {
  createApp,
  reactive,
  computed,
  nextTick,
} from "https://unpkg.com/vue@3.5.13/dist/vue.esm-browser.prod.js";

const ADJECTIVES = [
  "pretty","large","big","small","tall","short","long","handsome","plain","quaint",
  "clean","elegant","easy","angry","crazy","helpful","mushy","odd","unsightly",
  "adorable","important","inexpensive","cheap","expensive","fancy"
];
const COLOURS = ["red","yellow","blue","green","pink","brown","purple","brown","white","black","orange"];
const NOUNS = ["table","chair","house","bbq","desk","car","pony","cookie","sandwich","burger","pizza","mouse","keyboard"];

createApp({
  setup() {
    const state = reactive({
      rows: [],
      selectedId: null,
      nextId: 1,
      seed: 1,
      lastAction: "boot",
      lastDurationMs: "0.00",
    });

    function nextRand(max) {
      state.seed = (state.seed * 1664525 + 1013904223) >>> 0;
      return (state.seed >>> 16) % max;
    }

    function nextLabel() {
      return `${ADJECTIVES[nextRand(ADJECTIVES.length)]} ${COLOURS[nextRand(COLOURS.length)]} ${NOUNS[nextRand(NOUNS.length)]}`;
    }

    function makeRows(count) {
      const out = new Array(count);
      for (let i = 0; i < count; i++) {
        out[i] = { id: state.nextId++, label: nextLabel() };
      }
      return out;
    }

    function measure(action, fn) {
      const start = performance.now();
      fn();
      const end = performance.now();
      state.lastAction = action;
      state.lastDurationMs = (end - start).toFixed(2);
    }

    function run() {
      measure("run(1000)", () => {
        state.rows = makeRows(1000);
        state.selectedId = null;
      });
    }

    function runLots() {
      measure("runLots(10000)", () => {
        state.rows = makeRows(10000);
        state.selectedId = null;
      });
    }

    function add() {
      measure("add(1000)", () => {
        state.rows.push(...makeRows(1000));
      });
    }

    function updateEveryTenth() {
      measure("update every 10th", () => {
        for (let i = 0; i < state.rows.length; i += 10) {
          state.rows[i].label += " !!!";
        }
      });
    }

    function clear() {
      measure("clear", () => {
        state.rows = [];
        state.selectedId = null;
      });
    }

    function swapRows() {
      measure("swapRows", () => {
        if (state.rows.length > 998) {
          const tmp = state.rows[1];
          state.rows[1] = state.rows[998];
          state.rows[998] = tmp;
        }
      });
    }

    function selectRow(id) {
      measure("select", () => {
        state.selectedId = id;
      });
    }

    function removeRow(id) {
      measure("remove", () => {
        const idx = state.rows.findIndex((row) => row.id === id);
        if (idx >= 0) state.rows.splice(idx, 1);
        if (state.selectedId === id) state.selectedId = null;
      });
    }

    const selectedLabel = computed(() => {
      const row = state.rows.find((r) => r.id === state.selectedId);
      return row ? row.label : "none";
    });

    return {
      state,
      selectedLabel,
      run,
      runLots,
      add,
      updateEveryTenth,
      clear,
      swapRows,
      selectRow,
      removeRow,
    };
  },
  template: `
    <div class="container">
      <div class="jumbotron">
        <div class="row">
          <div class="col-md-6">
            <h1>vue keyed</h1>
          </div>
          <div class="col-md-6">
            <div class="row">
              <div class="col-sm-6 smallpad"><button id="run" type="button" @click="run">Create 1,000 rows</button></div>
              <div class="col-sm-6 smallpad"><button id="runlots" type="button" @click="runLots">Create 10,000 rows</button></div>
              <div class="col-sm-6 smallpad"><button id="add" type="button" @click="add">Append 1,000 rows</button></div>
              <div class="col-sm-6 smallpad"><button id="update" type="button" @click="updateEveryTenth">Update every 10th row</button></div>
              <div class="col-sm-6 smallpad"><button id="clear" type="button" @click="clear">Clear</button></div>
              <div class="col-sm-6 smallpad"><button id="swaprows" type="button" @click="swapRows">Swap Rows</button></div>
            </div>
          </div>
        </div>
        <div class="metrics">
          <span>Last action: <strong>{{ state.lastAction }}</strong></span>
          <span>Last duration: <strong>{{ state.lastDurationMs }}</strong> ms</span>
          <span>Rows: <strong>{{ state.rows.length }}</strong></span>
          <span>Selected: <strong>{{ selectedLabel }}</strong></span>
        </div>
      </div>

      <table class="table table-hover table-striped test-data">
        <tbody>
          <tr v-for="row in state.rows" :key="row.id" :class="state.selectedId === row.id ? 'danger' : ''">
            <td class="col-md-1">{{ row.id }}</td>
            <td class="col-md-4">
              <a @click="selectRow(row.id)">{{ row.label }}</a>
            </td>
            <td class="col-md-1">
              <a @click="removeRow(row.id)">
                <span class="glyphicon glyphicon-remove remove" aria-hidden="true"></span>
              </a>
            </td>
            <td class="col-md-6"></td>
          </tr>
        </tbody>
      </table>

      <span class="preloadicon" aria-hidden="true">×</span>
    </div>
  `,
}).mount("#app");
