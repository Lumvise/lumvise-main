import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const { instance } = await WebAssembly.instantiate(readFileSync(process.argv[2]), {});
const wasm = instance.exports;
const encoder = new TextEncoder();
const decoder = new TextDecoder();
const cases = JSON.parse(readFileSync(0, "utf8"));

function evaluate(input) {
  const bytes = encoder.encode(input);
  const pointer = wasm.revision_alloc(bytes.length);
  let outputPointer;
  let outputLength;
  try {
    new Uint8Array(wasm.memory.buffer, pointer, bytes.length).set(bytes);
    const output = wasm.revision_evaluate(pointer, bytes.length);
    outputPointer = Number(output & 0xffffffffn);
    outputLength = Number(output >> 32n);
    return decoder.decode(new Uint8Array(wasm.memory.buffer, outputPointer, outputLength).slice());
  } finally {
    if (outputPointer !== undefined) wasm.revision_free(outputPointer, outputLength);
    wasm.revision_free(pointer, bytes.length);
  }
}

for (let repetition = 0; repetition < 10; repetition += 1) {
  for (const { input, expected } of cases) assert.equal(evaluate(input), expected);
}
process.stdout.write(`${cases.length} native/WASM cases matched across 10 allocation cycles\n`);
