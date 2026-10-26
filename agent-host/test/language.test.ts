import { test } from "node:test";
import assert from "node:assert/strict";
import { languageLine } from "../src/host.ts";

test("la línea de idioma: nada sin configurar, el nombre si se conoce", () => {
  assert.equal(languageLine(undefined), "");
  assert.equal(languageLine(" "), "");
  assert.match(languageLine("es"), /^Always answer in Spanish/);
  assert.match(languageLine("nl"), /^Always answer in nl/);
});
