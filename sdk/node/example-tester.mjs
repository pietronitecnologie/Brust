// Example local software built with the Brust Node SDK: a test bench.
//
// A web app starts a test with
//   brust.call("tester", "startTest", { v: 1, code: "PZ-001", repetitions: 3 }, { timeout_ms: 60000 })
// receives "test.progress" events and gets back { code, outcome: "OK", measurements: [...] }.
//
//   BRUST_TOKEN=<token from Brust Settings> node example-tester.mjs [--name tester] [--port 47800]
import { parseArgs } from "node:util";
import { BrustClient } from "./brust-client.mjs";

const { values: args } = parseArgs({
  options: {
    token: { type: "string" },
    name: { type: "string", default: "tester" },
    port: { type: "string", default: "47800" },
  },
});

const client = new BrustClient({ token: args.token, name: args.name, port: Number(args.port), logger: console });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let busy = false; // the instrument runs one test at a time

client.method("startTest", async (params, ctx) => {
  params = params || {};
  if ((params.v ?? 1) !== 1) throw new Error(`unsupported version: ${params.v}`);
  const code = params.code;
  if (!code) throw new Error("missing 'code'");
  const repetitions = Number(params.repetitions ?? 1);
  if (!(repetitions >= 1 && repetitions <= 100)) throw new Error("'repetitions' must be between 1 and 100");
  if (busy) throw new Error("BUSY: test already running");
  busy = true;
  try {
    console.log(`test ${code} x${repetitions} requested by ${ctx.caller}`);
    const measurements = [];
    for (let i = 0; i < repetitions; i++) {
      await sleep(1000); // the real measurement goes here
      measurements.push(Math.round((12 + i * 0.1) * 1000) / 1000);
      await ctx.emitToCaller("test.progress", {
        code, step: i + 1, percent: Math.round(((i + 1) * 100) / repetitions),
      });
    }
    return { code, outcome: "OK", measurements };
  } finally {
    busy = false;
  }
});

client.method("status", () => ({ busy }));
client.onConnect((w) => console.log(`connected to Brust ${w.version ?? ""}, apps:`, w.apps));
client.on("*", (data, meta) => console.log(`event ${meta.event} from ${meta.source}:`, data));

client.start();
process.on("SIGINT", async () => { await client.stop(); process.exit(0); });
