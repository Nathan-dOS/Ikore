import { describe, it } from "node:test";
import assert from "node:assert/strict";
import tally from "./customs.js";

describe("Customs", () => {
    it("skips outside entries", () => {
        assert.equal(tally([0, 79, 80]), 79);
    });
});
